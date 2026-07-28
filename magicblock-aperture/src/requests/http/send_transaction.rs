use std::sync::{atomic::AtomicU64, Arc};

use magicblock_metrics::metrics::{
    TRANSACTION_PROCESSING_TIME, TRANSACTION_SKIP_PREFLIGHT,
};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_transaction_error::TransactionError;
use solana_transaction_status::UiTransactionEncoding;
use tracing::*;

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `sendTransaction` RPC request.
    ///
    /// Submits a new transaction to the validator's processing pipeline.
    /// The handler decodes and sanitizes the transaction, performs a robust
    /// replay-protection check, and then forwards it directly to the execution queue.
    #[instrument(skip_all)]
    pub(crate) async fn send_transaction(
        &self,
        request: &mut JsonRequest,
        remote_account_claims: Arc<AtomicU64>,
    ) -> HandlerResult {
        self.require_primary_rpc_method("sendTransaction")?;
        let _timer = TRANSACTION_PROCESSING_TIME.start_timer();
        let (transaction_str, config) =
            parse_params!(request.params()?, String, RpcSendTransactionConfig);

        let transaction_str: String = some_or_err!(transaction_str);
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);

        let transaction = self
            .prepare_transaction(&transaction_str, encoding, true, false)
            .inspect_err(
                |err| debug!(error = ?err, "Failed to prepare transaction"),
            )?;
        let signature = *transaction.txn.signature();

        // Perform a replay check and reserve the signature in the cache
        if self.transactions.contains(&signature)
            || !self.transactions.push(signature, None)
        {
            return Err(TransactionError::AlreadyProcessed.into());
        }

        let fetch_context =
            Self::send_transaction_context(signature, remote_account_claims);
        self.ensure_transaction_accounts(&transaction.txn, fetch_context)
            .await?;

        // Based on the preflight flag, either execute and await the result,
        // or schedule (fire-and-forget) for background processing.
        if config.skip_preflight {
            TRANSACTION_SKIP_PREFLIGHT.inc();
            self.transactions_scheduler.schedule(transaction).await?;
        } else {
            self.transactions_scheduler.execute(transaction).await?;
        }

        let signature = SerdeSignature(signature);
        Ok(ResponsePayload::encode_no_context(&request.id, signature))
    }
}
