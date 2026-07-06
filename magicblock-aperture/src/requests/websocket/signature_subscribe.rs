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

        // Subscribe first so no update can slip through between the status
        // check below and task startup.
        let mut rx = self
            .engine
            .transactions()
            .subscribe_signature(signature)
            .await;

        if let Some(status) = self
            .engine
            .transactions()
            .status(signature)
            .await
            .map_err(crate::error::RpcError::internal)?
        {
            if let Some(bytes) = encoder.encode(status.slot, &status.result, id)
            {
                let _ = self.chan.tx.send(bytes).await;
            }
            return Ok(SubResult::SubId(id));
        }

        let tx = self.chan.tx.clone();
        let handle = tokio::spawn(async move {
            if let Ok(status) = rx.recv().await
                && let Some(bytes) =
                    encoder.encode(status.slot, &status.result, id)
            {
                let _ = tx.send(bytes).await;
            }
        });
        self.register(id, handle);

        Ok(SubResult::SubId(id))
    }
}
