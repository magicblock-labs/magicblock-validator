use crate::{
    encoder::{Encoder, TransactionResultEncoder},
    requests::params::SerdeSignature,
    state::subscriptions::SubscriptionsDb,
};

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
        let id = SubscriptionsDb::next_subid();
        let status =
            self.transactions.get(&signature.0).flatten().and_then(|s| {
                TransactionResultEncoder.encode(s.slot, &s.result, id)
            });
        let (id, subscribed) = if let Some(payload) = status {
            let _ = self.chan.tx.send(payload).await;
            (id, Default::default())
        } else {
            self.subscriptions
                .subscribe_to_signature(signature.0, self.chan.clone())
                .await
        };
        self.signatures.push(signature.0, subscribed);
        Ok(SubResult::SubId(id))
    }
}
