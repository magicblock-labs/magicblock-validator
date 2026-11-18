use super::backlog::BacklogDB;
use crate::{service::CommittorServiceError, shared::outbox::outbox_client::InternalOutboxClient};
use engine::Engine;
use parking_lot::Mutex;
use program::outbox_intent_bundles::OutboxIntentBundle;
use solana_account::AccountMode;
use std::{num::NonZeroUsize, sync::Arc};
use tokio::{
    sync::mpsc::{Sender, error::TrySendError},
    task,
};
use tracing::error;

pub(crate) struct IntentQueue {
    pub engine: Engine,
    pub outbox_client: Arc<InternalOutboxClient>,
    pub backlog: Arc<Mutex<dyn BacklogDB>>,
    pub intent_sender: Sender<OutboxIntentBundle>,
}

impl IntentQueue {
    pub(crate) fn schedule(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> Result<(), IntentScheduleError> {
        if intent_bundles.is_empty() {
            return Ok(());
        }
        let backlog = self.backlog.lock();
        let schedule_result = if backlog.is_empty() {
            let mut iter = intent_bundles.into_iter();
            // Treated as regular value not propagated lower
            #[allow(clippy::result_large_err)]
            let send_result = iter.try_for_each(|bundle| self.intent_sender.try_send(bundle));
            match send_result {
                Ok(_) => Ok(()),
                Err(TrySendError::Closed(_)) => Err(IntentScheduleError::ChannelClosed),
                Err(TrySendError::Full(bundle)) => {
                    let leftovers = std::iter::once(bundle).chain(iter).collect();
                    backlog.store_intent_bundles(leftovers).map_err(IntentScheduleError::from)
                }
            }
        } else {
            backlog.store_intent_bundles(intent_bundles).map_err(IntentScheduleError::from)
        };

        schedule_result.inspect_err(|err| {
            error!(error = ?err, "Failed to schedule intent");
        })
    }
    pub(crate) async fn reschedule_intents(&self) -> Result<(), CommittorServiceError> {
        /// Number of intents rescheduled at once
        const RESCHEDULE_CHUNK_SIZE: NonZeroUsize = NonZeroUsize::new(1000).unwrap();

        let mut outbox_bundles_reader = self.outbox_client.outbox_reader();
        loop {
            // Read by chunks in order not to overload the execution worker
            let mut intent_bundles_chunk =
                outbox_bundles_reader.read(RESCHEDULE_CHUNK_SIZE.get()).await?;
            if intent_bundles_chunk.is_empty() {
                return Ok(());
            }

            let read_len = intent_bundles_chunk.len();
            intent_bundles_chunk.retain(|bundle| self.is_outbox_intent_recoverable(bundle));
            // Schedule  without initial persistence as bundle already exists in db
            let result = self.schedule(intent_bundles_chunk);
            if let Err(err) = result {
                error!(error = ?err, "Failed to reschedule pending bundles")
            }

            // Check if we've rescheduled intents from outbox
            if read_len != RESCHEDULE_CHUNK_SIZE.get() {
                return Ok(());
            }
        }
    }
    fn is_outbox_intent_recoverable(&self, bundle: &OutboxIntentBundle) -> bool {
        let accounts = bundle.get_all_committed_accounts();
        let store = self.engine.accounts();
        let loader = store.loader();
        let same_session = accounts.iter().all(|recovered| {
            loader
                .read(&recovered.pubkey, |account| {
                    matches!(
                        account.mode(),
                        AccountMode::Delegated | AccountMode::Undelegating
                    ) && (recovered.remote_slot == 0 || recovered.remote_slot == account.slot())
                })
                .ok()
                .flatten()
                .unwrap_or(false)
        });
        // Prepared task IDs must match their immutable scheduling reservations.
        same_session
            && bundle.recovery_commit_nonces().iter().all(|(pubkey, id)| {
                accounts
                    .iter()
                    .any(|account| account.pubkey == *pubkey && account.commit_id == *id)
            })
    }
}
#[derive(thiserror::Error, Debug)]
pub(crate) enum IntentScheduleError {
    #[error("Execution worker failed: {0}")]
    Worker(#[from] task::JoinError),
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("DBError: {0}")]
    DBError(#[from] super::backlog::Error),
}
