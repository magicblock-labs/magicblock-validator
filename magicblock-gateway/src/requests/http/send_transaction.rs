use log::warn;
use solana_message::SimpleAddressLoader;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_transaction::{
    sanitized::SanitizedTransaction,
    versioned::sanitized::SanitizedVersionedTransaction,
};
use solana_transaction_status::UiTransactionEncoding;
use tokio::sync::oneshot;

use crate::types::transactions::{
    ProcessableTransaction, TransactionProcessingMode,
};

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) async fn send_transaction(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (transaction, config) =
            parse_params!(params, String, RpcSendTransactionConfig);
        let transaction = transaction.ok_or_else(|| {
            RpcError::invalid_params("missing encoded transaction")
        });
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);
        unwrap!(transaction, request.id);
        let transaction = self.decode_transaction(&transaction, encoding);
        unwrap!(transaction, request.id);
        let signature = transaction.signatures[0];
        let hash = transaction.message.hash();
        let transaction = SanitizedVersionedTransaction::try_new(transaction)
            .map_err(RpcError::invalid_params);
        unwrap!(transaction, request.id);
        let transaction = SanitizedTransaction::try_new(
            transaction,
            hash,
            false,
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
        .map_err(RpcError::invalid_params);
        unwrap!(transaction, request.id);
        let _verification = transaction
            .verify()
            .map_err(RpcError::transaction_verification);
        unwrap!(_verification, request.id);
        let message = transaction.message();
        let reader = self.accountsdb.reader().map_err(RpcError::internal);
        unwrap!(reader, request.id);
        let mut ensured = false;
        loop {
            let mut to_ensure = Vec::new();
            let accounts = message.account_keys().iter().enumerate();
            for (index, pubkey) in accounts {
                if message.is_writable(index) {
                    match reader.read(pubkey, |account| account.delegated()) {
                        Some(true) => (),
                        Some(false) => {
                            let _err = Err(RpcError::invalid_params(
                                    "tried to use non-delegated account as writeable",
                            ));
                            unwrap!(_err, request.id);
                        }
                        None => to_ensure.push(*pubkey),
                    }
                } else if !reader.contains(pubkey) {
                    to_ensure.push(*pubkey);
                }
            }
            if ensured && !to_ensure.is_empty() {
                let _err = Err(RpcError::invalid_params(format!(
                    "transaction uses non-existent accounts: {to_ensure:?}"
                )));
                unwrap!(_err, request.id);
            }
            if to_ensure.is_empty() {
                break;
            }
            let to_ensure = AccountsToEnsure::new(to_ensure);
            let ready = to_ensure.ready.clone();
            let _ = self.ensure_accounts_tx.send(to_ensure).await;
            ready.notified().await;

            ensured = true;
        }

        let (result_tx, result_rx) = config
            .skip_preflight
            .then(|| oneshot::channel())
            .map(|(tx, rx)| (Some(tx), Some(rx)))
            .unwrap_or_default();
        let to_execute = ProcessableTransaction {
            transaction,
            mode: TransactionProcessingMode::Execution(result_tx),
        };
        if self
            .transaction_execution_tx
            .send(to_execute)
            .await
            .is_err()
        {
            warn!("transaction execution channel has closed");
        };
        if let Some(rx) = result_rx {
            if let Ok(result) = rx.await {
                let _result = result.map_err(RpcError::transaction_simulation);
                unwrap!(_result, request.id);
            }
        }
        let response =
            ResponsePayload::encode_no_context(&request.id, signature);
        Response::new(response)
    }
}
