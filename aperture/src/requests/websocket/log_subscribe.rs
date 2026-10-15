use json::Serialize;
use solana_svm::transaction_processing_result::TransactionProcessingResultExtensions;
use solana_transaction_error::TransactionError;

use super::prelude::*;
use crate::requests::{params::SerdeSignature, payload::NotificationPayload};

/// The value carried by a `logsNotification`.
#[derive(Serialize)]
struct LogsValue<'a> {
    signature: SerdeSignature,
    err: Option<TransactionError>,
    logs: &'a [String],
}

impl WsDispatcher {
    pub(crate) async fn logs_subscribe(&mut self, request: &JsonRequest) -> RpcResult<SubResult> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        enum LogFilter {
            #[serde(alias = "allWithVotes")]
            All,
            Mentions([Serde32Bytes; 1]),
        }

        let filter = request.required::<LogFilter>(0)?;

        let pubkey = match filter {
            LogFilter::Mentions([pubkey]) => pubkey.into(),
            LogFilter::All => {
                return Err(crate::error::RpcError::invalid_params(
                    "logsSubscribe 'all' filter is not supported",
                ));
            }
        };

        let id = next_subid();
        let rx = self.engine.transactions().subscribe_logs(pubkey).await;
        let engine = self.engine.clone();
        self.forward(id, rx, move |transaction| {
            let execution = transaction.execution.result.processed_transaction();
            let value = LogsValue {
                signature: SerdeSignature(transaction.transaction.signatures()[0]),
                err: transaction.execution.result.flattened_result().err(),
                logs: execution
                    .and_then(|execution| execution.execution_details.log_messages.as_deref())
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            };
            NotificationPayload::encode(value, context_slot(&engine), "logsNotification", id)
                .map(Some)
        });

        Ok(SubResult::SubId(id))
    }
}
