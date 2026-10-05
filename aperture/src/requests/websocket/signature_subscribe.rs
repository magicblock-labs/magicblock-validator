use super::prelude::*;
use crate::{encoder::encode_signature, requests::params::SerdeSignature};
use tracing::warn;

impl WsDispatcher {
    pub(crate) async fn signature_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
        let signature = request.required::<SerdeSignature>(0)?.into();

        let id = next_subid();

        let rx = self
            .engine
            .transactions()
            .subscribe_signature(signature)
            .await
            .map_err(crate::error::RpcError::internal)?;

        let tx = self.tx.clone();
        let engine = self.engine.clone();
        let handle = tokio::spawn(async move {
            if let Ok(status) = rx.await {
                match encode_signature(context_slot(&engine), &status.result, id) {
                    Ok(bytes) => {
                        let _ = tx.send(bytes).await;
                    }
                    Err(error) => {
                        warn!(?error, id, "signature notification serialization failed")
                    }
                }
            }
        });
        self.register(id, handle);

        Ok(SubResult::SubId(id))
    }
}
