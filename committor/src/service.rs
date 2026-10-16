use std::{sync::Arc, time::Duration};

use engine::Engine;
use mbv_core::intent::outbox::outbox_intent_pda_with_bump;
use metrics::metrics;
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use program::outbox_intent_bundles::OutboxIntentBundle;
use solana_account::AccountMode;
use solana_transaction::Transaction;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::{
    committor_processor::CommittorProcessor,
    error::CommittorServiceResult,
    intent_engine::db::BacklogDB,
    intent_executor::error::IntentExecutorError,
    outbox::{
        OutboxClient,
        outbox_client::InternalOutboxClientError,
        outbox_intent_bundles_reader::{OutboxIntentBundlesReader, OutboxIntentBundlesReaderError},
    },
    tasks::task_info_fetcher::AccountSnapshot,
};

/// Recovers durable intents from an already-restored Engine, then accepts new work.
pub struct IntentExecutionService<O, D> {
    engine: Engine,
    /// ER client specific for Intent needs. Could be switched to RpcClient
    outbox_client: Arc<O>,
    /// Processor of accepted intents
    processor: Arc<CommittorProcessor<D>>,
    /// Time interval to scrape MagicContext(ER slot interval)
    // TODO(edwin): can be removed if LatestBlocK moved into magicblock-core
    slot_interval: Duration,
}

impl<O, D: BacklogDB> IntentExecutionService<O, D>
where
    O: OutboxClient,
    // OutboxClient errors should be convertible to Service errors
    O::Error: Into<IntentExecutionServiceError>,
    // OutboxClient errors should be convertible into IntentExecutor errors
    O::Error: Into<IntentExecutorError>,
    // OutboxReader errors should be convertible to Service errors
    <O::OutboxReader as OutboxIntentBundlesReader>::Error: Into<IntentExecutionServiceError>,
{
    pub fn new(
        engine: Engine,
        outbox_client: Arc<O>,
        processor: Arc<CommittorProcessor<D>>,
        slot_interval: Duration,
    ) -> Self {
        Self {
            engine,
            outbox_client,
            processor,
            slot_interval,
        }
    }

    pub async fn run(self, mut shutdown: ShutdownHandle) {
        let result = self.accept_worker(shutdown.child()).await;
        let reason = match result {
            Err(error) => ShutdownReason::Error(Box::new(error)),
            Ok(()) if shutdown.requested() => ShutdownReason::Signalled,
            Ok(()) => ShutdownReason::Unexpected,
        };
        shutdown.terminate(reason);
    }

    async fn accept_worker(
        self,
        cancellation_token: CancellationToken,
    ) -> Result<(), IntentExecutionServiceError> {
        // Recover durable outbox work before accepting new intents.
        if let Err(err) = self.reschedule_intents().await {
            // TODO(edwin): alerts
            error!(error = ?err, "Failed to reschedule pending bundles")
        }

        let mut interval = tokio::time::interval(self.slot_interval);
        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    // Accepted intents are durable, so no drain is needed here.
                    break;
                }
                _ = interval.tick() => {
                    let accept_result = self
                        .outbox_client
                        .accept_scheduled_intents()
                        .await;
                    let intent_bundles = accept_result.unwrap_or_else(|(accepted_intents, err)| {
                        error!("Failed to accept intents: {}", err);
                        accepted_intents
                    });

                    let intent_bundles = intent_bundles.into_iter().map(|bundle| {
                        let bump =
                            outbox_intent_pda_with_bump(bundle.intent_id).1;
                        OutboxIntentBundle::accepted(bundle, bump)
                    }).collect();
                    if let Err(err) = self.schedule_intent_execution(intent_bundles).await {
                        error!("Failed to schedule intent execution: {}", err);
                    }
                }
            }
        }
        Ok(())
    }

    async fn reschedule_intents(&self) -> Result<(), IntentExecutionServiceError> {
        const BATCH_SIZE: usize = 1000;

        let mut reader = self.outbox_client.outbox_reader();
        loop {
            // Read by chunks in order not to overload `IntentExecutionEngine`
            let bundles = reader.read(BATCH_SIZE).await.map_err(Into::into)?;
            let read_len = bundles.len();
            if bundles.is_empty() {
                return Ok(());
            }

            // Original blockhash is stale after restart; signal recovery so
            // notify_commit_sent rebuilds the tx with a fresh ER blockhash.
            let mut recoverable = Vec::with_capacity(read_len);
            for mut bundle in bundles {
                bundle.inner.sent_transaction = Transaction::default();
                if self.recoverable(&bundle).await {
                    recoverable.push(bundle);
                }
            }
            // Schedule without initial persistence as bundles already exist in db.
            if !recoverable.is_empty()
                && let Err(err) = self.processor.schedule_intent_bundles(recoverable).await
            {
                error!(error = ?err, "Failed to reschedule pending bundles")
            }

            if read_len != BATCH_SIZE {
                return Ok(());
            }
        }
    }

    async fn schedule_intent_execution(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> CommittorServiceResult<()> {
        if intent_bundles.is_empty() {
            return Ok(());
        }

        metrics::inc_committor_intents_count_by(intent_bundles.len() as u64);

        self.processor.schedule_intent_bundles(intent_bundles).await
    }

    async fn recoverable(&self, bundle: &OutboxIntentBundle) -> bool {
        let accounts = bundle
            .get_all_committed_accounts()
            .into_iter()
            .map(|account| (account.pubkey, account.remote_slot))
            .collect::<Vec<AccountSnapshot>>();
        self.same_session(bundle.intent_id, &accounts) && self.valid_nonces(bundle, &accounts).await
    }

    /// Recovery uses authoritative state already restored by Engine; missing accounts are not refetched.
    fn same_session(&self, intent_id: u64, snapshots: &[AccountSnapshot]) -> bool {
        let accounts = self.engine.accounts();
        let loader = accounts.loader();
        for &(pubkey, recovered_slot) in snapshots {
            let current =
                loader.read(&pubkey, |account| (account.mode(), account.slot())).ok().flatten();
            let Some((mode, current_slot)) = current else {
                error!(intent_id, %pubkey,
                    "Skipping outbox recovery because committed account is missing locally");
                return false;
            };
            let locally_protected = matches!(mode, AccountMode::Delegated | AccountMode::Transient);
            if !locally_protected || (recovered_slot != 0 && recovered_slot != current_slot) {
                error!(intent_id, %pubkey, recovered_slot, current_slot, locally_protected,
                    "Skipping outbox recovery because delegation session changed");
                return false;
            }
        }
        true
    }

    async fn valid_nonces(
        &self,
        bundle: &OutboxIntentBundle,
        accounts: &[AccountSnapshot],
    ) -> bool {
        let recovery_nonces = bundle.recovery_commit_nonces();
        if recovery_nonces.is_empty() {
            return true;
        }

        let min_context_slot = accounts.iter().map(|(_, slot)| *slot).max().unwrap_or_default();
        let current_nonces =
            match self.processor.fetch_current_commit_nonces(accounts, min_context_slot).await {
                Ok(nonces) => nonces,
                Err(err) => {
                    error!(
                        intent_id = bundle.intent_id,
                        error = ?err,
                        "Skipping outbox recovery after commit nonce lookup failed"
                    );
                    return false;
                }
            };

        for &(pubkey, recovery_commit_nonce) in recovery_nonces {
            let Some(&current_commit_nonce) = current_nonces.get(&pubkey) else {
                error!(
                    intent_id = bundle.intent_id,
                    %pubkey,
                    "Skipping outbox recovery because current commit nonce is missing"
                );
                return false;
            };
            if recovery_commit_nonce < current_commit_nonce {
                error!(
                    intent_id = bundle.intent_id,
                    %pubkey,
                    recovery_commit_nonce,
                    current_commit_nonce,
                    "Skipping stale outbox recovery because chain nonce has advanced"
                );
                return false;
            }
        }
        true
    }
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutionServiceError {
    #[error("Intent execution worker '{0}' stopped unexpectedly")]
    WorkerStopped(&'static str),
    #[error("IntentRpcClientError: {0}")]
    IntentRpcClientError(#[from] InternalOutboxClientError),
    #[error("OutboxReaderError")]
    OutboxReaderError(#[from] OutboxIntentBundlesReaderError),
    #[error("IntentExecutorError: {0}")]
    IntentExecutorError(#[from] Box<IntentExecutorError>),
}
