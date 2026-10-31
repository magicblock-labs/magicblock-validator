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
            metrics::metrics::inc_committor_intents_count_by(intent_bundles.len() as u64);
            if let Err(err) = self.queue.schedule(intent_bundles).await {
                error!("Failed to schedule intent execution: {}", err);
            }
        }
    }
}
