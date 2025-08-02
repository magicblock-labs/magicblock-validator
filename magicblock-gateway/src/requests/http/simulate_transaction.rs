use hyper::Response;
use log::warn;
use magicblock_gateway_types::{
    accounts::AccountsToEnsure,
    transactions::{ProcessableTransaction, TransactionProcessingMode},
};
use solana_message::SimpleAddressLoader;
use solana_rpc_client_api::{
    config::RpcSimulateTransactionConfig,
    response::{RpcBlockhash, RpcSimulateTransactionResult},
};
use solana_transaction::{
    sanitized::SanitizedTransaction,
    versioned::sanitized::SanitizedVersionedTransaction,
};
use solana_transaction_status::UiTransactionEncoding;
use tokio::sync::oneshot;

use crate::{
    error::RpcError,
    requests::{payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) async fn simulate_transaction(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (transaction, config) =
            parse_params!(params, String, RpcSimulateTransactionConfig);
        let transaction = transaction.ok_or_else(|| {
            RpcError::invalid_params("missing encoded transaction")
        });
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);
        unwrap!(transaction, request.id);
        let transaction = self.decode_transaction(&transaction, encoding);
        unwrap!(transaction, request.id);
        let (hash, replacement) = if config.replace_recent_blockhash {
            let latest = self.blocks.get_latest();
            (latest.hash, Some(RpcBlockhash::from(latest)))
        } else {
            (transaction.message.hash(), None)
        };
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
        if config.sig_verify {
            let _verification = transaction
                .verify()
                .map_err(RpcError::transaction_verification);
            unwrap!(_verification, request.id);
        }
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

        let (result_tx, result_rx) = oneshot::channel();
        let to_execute = ProcessableTransaction {
            transaction,
            mode: TransactionProcessingMode::Simulation {
                result_tx,
                inner_instructions: config.inner_instructions,
            },
        };
        if self
            .transaction_execution_tx
            .send(to_execute)
            .await
            .is_err()
        {
            warn!("transaction execution channel has closed");
        };
        let result = result_rx.await.map_err(RpcError::transaction_simulation);
        unwrap!(result, request.id);
        let slot = self.accountsdb.slot();
        let result = RpcSimulateTransactionResult {
            err: result.result.err(),
            logs: Some(result.logs.to_vec()),
            accounts: None,
            units_consumed: Some(result.units_consumed),
            return_data: result.return_data.map(Into::into),
            inner_instructions: result.inner_instructions.map(|ii| {
                IntoIterator::into_iter(ii).map(Into::into).collect()
            }),
            replacement_blockhash: replacement,
        };
        ResponsePayload::encode(&request.id, result, slot)
    }
}
