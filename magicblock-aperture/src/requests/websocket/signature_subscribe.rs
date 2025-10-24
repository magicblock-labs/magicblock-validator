use super::prelude::*;
use crate::{
    encoder::{Encoder, TransactionResultEncoder},
    requests::params::SerdeSignature,
    some_or_err,
    state::subscriptions::SubscriptionsDb,
};

impl WsDispatcher {
    /// Handles the `signatureSubscribe` WebSocket RPC request.
    ///
    /// Creates a one-shot subscription for a transaction signature. The handler
    /// first performs a fast-path check against a cache of recent transactions.
    /// If the transaction is already finalized, the notification is sent
    /// immediately. Otherwise, it registers a subscription that will either be
    /// fulfilled when the transaction is processed or automatically expire.
    pub(crate) async fn signature_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let signature = parse_params!(request.params()?, SerdeSignature);
        let signature = some_or_err!(signature);

        let sub_id = SubscriptionsDb::next_subid();

        // Fast path: Check if the transaction result is already in the cache.
        let cached_status =
            self.transactions.get(&signature).flatten().and_then(|s| {
                TransactionResultEncoder.encode(s.slot, &s.result, sub_id)
            });

        let (id, subscribed) = if let Some(payload) = cached_status {
            // If already cached, send the notification immediately without creating
            // a persistent subscription.
            let _ = self.chan.tx.send(payload).await;
            (sub_id, Default::default())
        } else {
            // Otherwise, register a new one-shot subscription.
            self.subscriptions
                .subscribe_to_signature(signature, self.chan.clone())
                .await
        };

        // Track the subscription in the per-connection expirer to prevent leaks.
        self.signatures.push(signature, subscribed);
        Ok(SubResult::SubId(id))
    }
}
