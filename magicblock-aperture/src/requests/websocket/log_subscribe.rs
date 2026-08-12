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
    pub(crate) async fn logs_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
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
        let mut rx = self.engine.transactions().subscribe_logs(pubkey).await;
        let tx = self.chan.tx.clone();
        let engine = self.engine.clone();
        let handle = tokio::spawn(async move {
            while let Some(logs) = rx.recv().await {
                let slot = engine.blocks().latest().slot;
                let value = LogsValue {
                    signature: logs.signature.to_string(),
                    err: logs.result.as_ref().err().cloned(),
                    logs: logs.logs.as_ref().clone(),
                };
                let Some(bytes) = NotificationPayload::encode(
                    value,
                    slot,
                    "logsNotification",
                    id,
                ) else {
                    continue;
                };
                if tx.send(bytes).await.is_err() {
                    break;
                }
            }
        });
        self.register(id, handle);

        Ok(SubResult::SubId(id))
    }
}
