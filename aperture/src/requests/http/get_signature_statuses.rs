use solana_rpc_client_api::request::MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS;

use super::{HandlerResult, RpcHandlers};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::SerdeSignature, payload::ResponsePayload},
};

impl RpcHandlers {
    pub(crate) async fn get_signature_statuses(&self, request: &JsonRequest) -> HandlerResult {
        let signatures = request.required::<Vec<SerdeSignature>>(0)?;
        if signatures.len() > MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS {
            return Err(RpcError::invalid_params(
                "too many signatures were requested, max allowed: 256",
            ));
        }
        let signatures = signatures.into_iter().map(Into::into).collect::<Vec<_>>();
        let statuses = self.history.statuses(&signatures).await?;

        let slot = self.engine.blocks().latest().slot;
        ResponsePayload::encode(&request.id, statuses, slot)
    }
}
