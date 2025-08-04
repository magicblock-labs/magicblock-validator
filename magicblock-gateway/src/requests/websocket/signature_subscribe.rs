use crate::requests::params::SerdeSignature;

use super::prelude::*;

impl WsDispatcher {
    pub(crate) async fn signature_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let mut params = request
            .params
            .take()
            .ok_or_else(|| RpcError::invalid_request("missing params"))?;

        let signature = parse_params!(params, SerdeSignature);
        let signature = signature.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid signature")
        })?;
        let (id, subscribed) = self
            .subscriptions
            .subscribe_to_signature(signature.0, self.chan.clone())
            .await;
        self.signatures.push(signature.0, subscribed);
        Ok(SubResult::SubId(id))
    }
}
