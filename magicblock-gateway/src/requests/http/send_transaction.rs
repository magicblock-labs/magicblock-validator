use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_transaction_error::TransactionError;
use solana_transaction_status::UiTransactionEncoding;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) async fn send_transaction(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (transaction, config) =
            parse_params!(request.params()?, String, RpcSendTransactionConfig);
        let transaction = transaction.ok_or_else(|| {
            RpcError::invalid_params("missing encoded transaction")
        })?;
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);
        let transaction =
            self.prepare_transaction(&transaction, encoding, false, false)?;
        let signature = *transaction.signature();

        // check whether signature has been processed recently, if not then reserve
        // the cache entry for it to prevent rapid double spending attacks. This means
        // that only one transaction with a given signature can be processed within
        // the cache expiration period (which is equal to blockhash validity time)
        if self.transactions.contains(&signature)
            || !self.transactions.push(signature, None)
        {
            Err(TransactionError::AlreadyProcessed)?;
        }

        self.ensure_transaction_accounts(&transaction).await?;

        if config.skip_preflight {
            self.transactions_scheduler.schedule(transaction).await?;
        } else {
            self.transactions_scheduler.execute(transaction).await?;
        }
        Ok(ResponsePayload::encode_no_context(&request.id, signature))
    }
}
