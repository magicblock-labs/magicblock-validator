use super::prelude::*;
use crate::{
    encoder::TransactionResultEncoder, requests::params::SerdeSignature,
};

impl WsDispatcher {
    pub(crate) async fn signature_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
        let signature = request.required::<SerdeSignature>(0)?.into();

        let id = next_subid();
        let encoder = TransactionResultEncoder;

        let rx = self
            .engine
            .transactions()
            .subscribe_signature(signature)
            .await
            .map_err(crate::error::RpcError::internal)?;

        let tx = self.chan.tx.clone();
        let engine = self.engine.clone();
        let handle = tokio::spawn(async move {
            if let Ok(status) = rx.await
                && let Some(bytes) =
                    encoder.encode(context_slot(&engine), &status.result, id)
            {
                let _ = tx.send(bytes).await;
            }
        });
        self.register(id, handle);

        Ok(SubResult::SubId(id))
    }
}
