use json::Deserialize;

use super::prelude::*;
use crate::{encoder::TransactionLogsEncoder, some_or_err};

impl WsDispatcher {
    /// Handles the `logsSubscribe` WebSocket RPC request.
    ///
    /// Registers the current WebSocket connection to receive transaction logs.
    /// The subscription can be filtered to either receive all logs (`"all"`) or
    /// only logs from transactions that mention a specific account pubkey.
    pub(crate) fn logs_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        // A local enum to deserialize the first parameter of the logsSubscribe request.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        enum LogFilter {
            #[serde(alias = "allWithVotes")]
            All,
            Mentions([Serde32Bytes; 1]),
        }

        let filter = parse_params!(request.params()?, LogFilter);
        let filter = some_or_err!(filter);

        // Convert the RPC filter into the internal encoder representation.
        let encoder = match filter {
            LogFilter::All => TransactionLogsEncoder::All,
            LogFilter::Mentions([pubkey]) => {
                TransactionLogsEncoder::Mentions(pubkey.into())
            }
        };

        let handle = self
            .subscriptions
            .subscribe_to_logs(encoder, self.chan.clone());

        self.unsubs.insert(handle.id, handle.cleanup);
        Ok(SubResult::SubId(handle.id))
    }
}
