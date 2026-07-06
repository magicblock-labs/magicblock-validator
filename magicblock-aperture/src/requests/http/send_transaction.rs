use magicblock_metrics::metrics::{
    TRANSACTION_PROCESSING_TIME, TRANSACTION_SKIP_PREFLIGHT,
};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
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
    ) -> HandlerResult {
        self.require_primary_rpc_method("sendTransaction")?;
        let _timer = TRANSACTION_PROCESSING_TIME.start_timer();
        let (transaction_str, config) =
            parse_params!(request.params()?, String, RpcSendTransactionConfig);

        let transaction_str: String = some_or_err!(transaction_str);
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);

        let transaction = self
            .decode_transaction(&transaction_str, encoding)
            .inspect_err(
            |err| debug!(error = ?err, "Failed to decode transaction"),
        )?;
        let signature = transaction.signatures()[0];

        self.ensure_transaction_accounts(&transaction).await?;

        // Hand the raw payload to the engine, which owns replay protection,
        // signature verification and blockhash validation. Based on the
        // preflight flag, either execute and await the committed result, or
        // schedule (fire-and-forget) for background processing.
        if config.skip_preflight {
            TRANSACTION_SKIP_PREFLIGHT.inc();
            self.engine.transaction(transaction)?.schedule().await?;
        } else {
            self.engine.transaction(transaction)?.execute().await??;
        }

        let signature = SerdeSignature(signature);
        Ok(ResponsePayload::encode_no_context(&request.id, signature))
    }
}
