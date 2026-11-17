use crate::{execution::queue::IntentQueue, shared::outbox::outbox_client::InternalOutboxClient};
use mbv_core::intent::outbox::outbox_intent_pda_with_bump;
use program::outbox_intent_bundles::OutboxIntentBundle;
use std::{sync::Arc, time::Duration};
use tracing::error;

pub(crate) struct IntentIntake {
    pub outbox_client: Arc<InternalOutboxClient>,
    pub queue: Arc<IntentQueue>,
    pub slot_interval: Duration,
}

impl IntentIntake {
    pub(crate) async fn run(self) {
        let mut interval = tokio::time::interval(self.slot_interval);
        loop {
            interval.tick().await;
            let accept_result = self.outbox_client.accept_scheduled_intents().await;
            let intent_bundles = accept_result.unwrap_or_else(|(accepted_intents, err)| {
                error!("Failed to accept intents: {}", err);
                accepted_intents
            });

            let intent_bundles = intent_bundles
                .into_iter()
                .map(|bundle| {
                    let bump = outbox_intent_pda_with_bump(bundle.intent_id).1;
                    OutboxIntentBundle::accepted(bundle, bump)
                })
                .collect::<Vec<_>>();
            crate::metrics::scheduled(intent_bundles.len());
            if let Err(err) = self.queue.schedule(intent_bundles) {
                error!("Failed to schedule intent execution: {}", err);
            }
        }
    }
}
