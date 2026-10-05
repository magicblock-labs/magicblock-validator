use super::{HandlerResult, RpcHandlers};
use crate::{
    account::TransactionKind,
    requests::{JsonHttpRequest as JsonRequest, params::SerdeSignature, payload::ResponsePayload},
};
use magicblock_metrics::metrics::{TRANSACTION_PROCESSING_TIME, TRANSACTION_SKIP_PREFLIGHT};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_transaction_status::UiTransactionEncoding;
impl RpcHandlers {
    pub(crate) async fn send_transaction(
        &self,
        request: &JsonRequest,
        claims: &mut u64,
    ) -> HandlerResult {
        let _timer = TRANSACTION_PROCESSING_TIME.start_timer();
        let transaction_str = request.required::<&str>(0)?;
        let config = request.optional::<RpcSendTransactionConfig>(1)?.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);

        let transaction = self
            .accounts
            .prepare_transaction(transaction_str, encoding, TransactionKind::Send, claims)
            .await?;
        let signature = transaction.signatures()[0];

        if config.skip_preflight {
            TRANSACTION_SKIP_PREFLIGHT.inc();
            self.engine.transaction(transaction)?.schedule().await?;
        } else {
            self.engine.transaction(transaction)?.execute().await??;
        }

        let signature = SerdeSignature(signature);
        ResponsePayload::encode_no_context(&request.id, signature)
    }
}
