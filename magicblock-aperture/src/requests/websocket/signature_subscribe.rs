use super::prelude::*;
use crate::{
    encoder::TransactionResultEncoder, requests::params::SerdeSignature,
    some_or_err,
};

impl WsDispatcher {
    /// Handles the `signatureSubscribe` WebSocket RPC request.
    ///
    /// Creates a one-shot subscription for a transaction signature. It first
    /// opens the engine status subscription, then checks the engine for an
    /// already-settled status (closing the race between the two). If the status
    /// is already known it is sent immediately; otherwise a task awaits the first
    /// status update, forwards it, and terminates.
    pub(crate) async fn signature_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let signature = parse_params!(request.params()?, SerdeSignature);
        let signature = some_or_err!(signature);

        let id = next_subid();
        let encoder = TransactionResultEncoder;

        // Subscribe first so no update can slip through between the status
        // check below and task startup.
        let mut rx = self
            .engine
            .transactions()
            .subscribe_signature(signature)
            .await;

        // Fast path: the status may already be settled.
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
