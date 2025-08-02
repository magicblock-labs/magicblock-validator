use json::Deserialize;

use crate::{
    encoder::TransactionLogsEncoder,
    error::RpcError,
    requests::{params::Serde32Bytes, JsonRequest},
    server::websocket::dispatch::{SubResult, WsDispatcher},
    RpcResult,
};

impl WsDispatcher {
    pub(crate) fn logs_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let mut params = request
            .params
            .take()
            .ok_or_else(|| RpcError::invalid_request("missing params"))?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        enum LogFilter {
            #[serde(alias = "allWithVotes")]
            All,
            Mentions([Serde32Bytes; 1]),
        }

        let filter = parse_params!(params, LogFilter);
        let filter = filter.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid log filter")
        })?;
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
