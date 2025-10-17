use log::*;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_transaction_error::TransactionError;
use solana_transaction_status::UiTransactionEncoding;

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `sendTransaction` RPC request.
    ///
    /// Submits a new transaction to the validator's processing pipeline.
    /// The handler decodes and sanitizes the transaction, performs a robust
    /// replay-protection check, and then forwards it directly to the execution queue.
    pub(crate) async fn send_transaction(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (transaction_str, config) =
            parse_params!(request.params()?, String, RpcSendTransactionConfig);
        let transaction_str: String = some_or_err!(transaction_str);
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);

        let transaction = self
            .prepare_transaction(&transaction_str, encoding, true, false)
            .inspect_err(|err| {
                error!(
                    "Failed to prepare transaction: {transaction_str} ({err})"
                )
            })?;
        let signature = *transaction.signature();

        // Perform a replay check and reserve the signature in the cache. This prevents
        // a transaction from being processed twice within the blockhash validity period.
        if self.transactions.contains(&signature)
            || !self.transactions.push(signature, None)
        {
            return Err(TransactionError::AlreadyProcessed.into());
        }
        debug!("Received transaction: {signature}, ensuring accounts");
        self.ensure_transaction_accounts(&transaction).await?;

        // Based on the preflight flag, either execute and await the result,
        // or schedule (fire-and-forget) for background processing.
        if config.skip_preflight {
            trace!("Scheduling transaction: {signature}");
            self.transactions_scheduler.schedule(transaction).await?;
        } else {
            trace!("Executing transaction: {signature}");
            self.transactions_scheduler.execute(transaction).await?;
        }

        let signature = SerdeSignature(signature);
        Ok(ResponsePayload::encode_no_context(&request.id, signature))
    }
}
