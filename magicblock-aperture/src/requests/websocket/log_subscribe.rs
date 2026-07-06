use json::Serialize;
use solana_transaction_error::TransactionError;

use super::prelude::*;
use crate::{requests::payload::NotificationPayload, some_or_err};

/// The value carried by a `logsNotification`.
#[derive(Serialize)]
struct LogsValue {
    signature: String,
    err: Option<TransactionError>,
    logs: Vec<String>,
}

impl WsDispatcher {
    /// Handles the `logsSubscribe` WebSocket RPC request.
    ///
    /// Only the `mentions` filter is supported: the subscription forwards logs
    /// from transactions that mention a specific account pubkey. Spawns a task
    /// that encodes each engine log batch inline and forwards it to this
    /// connection.
    pub(crate) async fn logs_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        // A local enum to deserialize the first parameter of the logsSubscribe request.
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        enum LogFilter {
            #[serde(alias = "allWithVotes")]
            All,
            Mentions([Serde32Bytes; 1]),
        }

        let filter = parse_params!(request.params()?, LogFilter);
        let filter = some_or_err!(filter);

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
            while let Ok(logs) = rx.recv().await {
                // The log batch carries no slot; derive it from the latest block.
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
