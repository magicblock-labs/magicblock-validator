use json::Serialize;
use solana_transaction_error::TransactionError;

use super::prelude::*;
use crate::requests::payload::NotificationPayload;

/// The value carried by a `logsNotification`.
#[derive(Serialize)]
struct LogsValue {
    signature: String,
    err: Option<TransactionError>,
    logs: Vec<String>,
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
        self.forward(id, rx, move |logs| {
            let value = LogsValue {
                signature: logs.signature.to_string(),
                err: logs.result.as_ref().err().cloned(),
                logs: logs.logs.as_ref().clone(),
            };
            NotificationPayload::encode(value, context_slot(&engine), "logsNotification", id)
                .map(Some)
        });

        Ok(SubResult::SubId(id))
    }
}
