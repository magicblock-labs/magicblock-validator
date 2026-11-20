use super::prelude::*;
use crate::{encoder::encode_signature, requests::params::SerdeSignature};
use tracing::warn;

impl WsDispatcher {
    pub(crate) async fn signature_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
        let signature = request.required::<SerdeSignature>(0)?.into();

        let rx = self.engine.transactions().subscribe_signature(signature).await;
        let engine = self.engine.clone();
        Ok(self.spawn_subscription(move |id, tx| async move {
            let Ok(status) = rx.await else { return };
            match encode_signature(context_slot(&engine), &status.result, id) {
                Ok(bytes) => {
                    let _ = tx.send(bytes).await;
                }
                Err(error) => {
                    warn!(?error, "signature notification serialization failed");
                }
            }
        }))
    }
}
