use std::{
    collections::{hash_map::Entry, HashMap},
    path::Path,
    sync::{atomic::AtomicU64, Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use futures_util::future::join_all;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use tokio::sync::{broadcast, oneshot, oneshot::error::RecvError};
use tracing::{error, info, instrument};

use crate::{
    config::ChainConfig,
    error::{CommittorServiceError, CommittorServiceResult},
    intent_execution_manager::{
        db::DummyDB, BroadcastedIntentExecutionResult, IntentExecutionManager,
    },
    intent_executor::{
        intent_executor_factory::ExecutorConfig,
        task_info_fetcher::{
            CacheTaskInfoFetcher, RpcTaskInfoFetcher, TaskInfoFetcher,
            TaskInfoFetcherResult,
        },
    },
    persist::{
        CommitStatusRow, IntentPersister, IntentPersisterImpl,
        MessageSignatures,
    },
};
const POISONED_MUTEX_MSG: &str =
    "CommittorProcessor pending messages mutex poisoned!";
type BundleResultListener = oneshot::Sender<BroadcastedIntentExecutionResult>;

pub struct CommittorProcessor {
    authority: Keypair,
    _table_mania: TableMania,
    magic_rpc_client: MagicblockRpcClient,
    persister: IntentPersisterImpl,
    commits_scheduler: IntentExecutionManager<DummyDB>,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    pending_result_listeners: Arc<Mutex<HashMap<u64, BundleResultListener>>>,
}

impl CommittorProcessor {
    pub fn try_new<P, A>(
        authority: Keypair,
        persist_file: P,
        chain_config: ChainConfig,
        chain_slot: Option<Arc<AtomicU64>>,
        actions_callback_executor: A,
    ) -> CommittorServiceResult<Self>
    where
        P: AsRef<Path>,
        A: ActionsCallbackScheduler,
    {
        let rpc_client = RpcClient::new_with_commitment(
            chain_config.rpc_uri.to_string(),
            chain_config.commitment,
        );
        let rpc_client = Arc::new(rpc_client);
        let magic_block_rpc_client = if let Some(chain_slot) = chain_slot {
            MagicblockRpcClient::new_with_chain_slot(rpc_client, chain_slot)
        } else {
            MagicblockRpcClient::new(rpc_client)
        };

        // Create TableMania
        let gc_config = GarbageCollectorConfig::default();
        let table_mania = TableMania::new(
            magic_block_rpc_client.clone(),
            &authority,
            Some(gc_config),
        );

        // Create commit persister
        let persister = IntentPersisterImpl::try_new(persist_file)?;

        // Create commit scheduler
        let task_info_fetcher = Arc::new(CacheTaskInfoFetcher::new(
            RpcTaskInfoFetcher::new(magic_block_rpc_client.clone()),
        ));
        let commits_scheduler = IntentExecutionManager::new(
            magic_block_rpc_client.clone(),
            DummyDB::new(),
            task_info_fetcher.clone(),
            Some(persister.clone()),
            table_mania.clone(),
            ExecutorConfig {
                compute_budget_config: chain_config
                    .compute_budget_config
                    .clone(),
                actions_timeout: chain_config.actions_timeout,
            },
            actions_callback_executor,
        );

        let result_subscription = commits_scheduler.subscribe_for_results();
        let pending_result_listeners = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(Self::dispatcher(
            result_subscription,
            pending_result_listeners.clone(),
        ));

        Ok(Self {
            authority,
            _table_mania: table_mania,
            magic_rpc_client: magic_block_rpc_client,
            commits_scheduler,
            persister,
            task_info_fetcher,
            pending_result_listeners,
        })
    }

    pub fn get_commit_statuses(
        &self,
        message_id: u64,
    ) -> CommittorServiceResult<Vec<CommitStatusRow>> {
        let commit_statuses =
            self.persister.get_commit_statuses_by_message(message_id)?;
        Ok(commit_statuses)
    }

    pub fn get_commit_signature(
        &self,
        commit_id: u64,
        pubkey: Pubkey,
    ) -> CommittorServiceResult<Option<MessageSignatures>> {
        let signatures = self
            .persister
            .get_signatures_by_commit(commit_id, &pubkey)?;
        Ok(signatures)
    }

    /// Fetches pending bundles from DB
    pub async fn pending_intent_bundles(
        &self,
    ) -> CommittorServiceResult<Vec<ScheduledIntentBundle>> {
        const RECOVERY_MIN_AGE_SECS: u64 = 30 * 60;

        // Extract pending bundles satisfying predicate
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut bundles = self.persister.pending_intent_bundles(|row| {
            now.saturating_sub(row.last_retried_at) <= RECOVERY_MIN_AGE_SECS
        })?;

        if bundles.is_empty() {
            return Ok(bundles);
        }

        // Log info about extracted bundles
        {
            let accounts_count: usize = bundles
                .iter()
                .map(|bundle| bundle.get_all_committed_pubkeys().len())
                .sum();
            info!(
                intent_count = bundles.len(),
                accounts_count,
                "Loaded pending commit intents from persistence for recovery"
            );
        }

        // Extracted bundles are out of data and missing some of the info
        self.refresh_intent_bundles(&mut bundles).await?;

        Ok(bundles)
    }

    async fn refresh_intent_bundles(
        &self,
        intent_bundles: &mut [ScheduledIntentBundle],
    ) -> CommittorServiceResult<()> {
        let payer = self.authority.pubkey();
        let slot = self.magic_rpc_client.get_slot().await?;

        macro_rules! set_remote_slot {
            ($field:expr, $slot:expr) => {
                if let Some(ref mut v) = $field {
                    v.get_committed_accounts_mut()
                        .iter_mut()
                        .for_each(|a| a.remote_slot = slot);
                }
            };
        }

        intent_bundles.iter_mut().for_each(|b| {
            b.payer = payer;
            set_remote_slot!(b.intent_bundle.commit, slot);
            set_remote_slot!(b.intent_bundle.commit_finalize, slot);
            set_remote_slot!(b.intent_bundle.commit_and_undelegate, slot);
            set_remote_slot!(
                b.intent_bundle.commit_finalize_and_undelegate,
                slot
            );
        });

        Ok(())
    }

    #[instrument(skip(self, intent_bundles))]
    pub async fn schedule_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> CommittorServiceResult<()> {
        if let Err(err) = self.persister.start_base_intents(&intent_bundles) {
            // We will still try to perform the commits, but the fact that we cannot
            // persist the intent is very serious and we should probably restart the
            // valiator
            error!(error = ?err, "DB EXCEPTION: Failed to persist changeset");
        };

        self.commits_scheduler
            .schedule(intent_bundles)
            .await
            .inspect_err(|err| {
                error!(error = ?err, "Failed to schedule intent");
            })?;

        Ok(())
    }

    pub async fn execute_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> CommittorServiceResult<Vec<BroadcastedIntentExecutionResult>> {
        // Critical section
        let receivers = {
            let mut result_listeners = self
                .pending_result_listeners
                .lock()
                .expect(POISONED_MUTEX_MSG);

            intent_bundles
                .iter()
                .map(|intent| {
                    let (sender, receiver) = oneshot::channel();
                    match result_listeners.entry(intent.id) {
                        Entry::Vacant(vacant) => {
                            vacant.insert(sender);
                            Ok(receiver)
                        }
                        Entry::Occupied(_) => {
                            Err(CommittorServiceError::RepeatingMessageError(
                                intent.id,
                            ))
                        }
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        self.schedule_intent_bundles(intent_bundles).await?;
        let results = join_all(receivers.into_iter())
            .await
            .into_iter()
            .collect::<Result<Vec<_>, RecvError>>()?;

        Ok(results)
    }

    #[instrument(skip(self, intent_bundles))]
    pub async fn schedule_recovered_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> CommittorServiceResult<()> {
        self.commits_scheduler
            .schedule(intent_bundles)
            .await
            .inspect_err(|err| {
                error!(error = ?err, "Failed to schedule recovered intent");
            })?;

        Ok(())
    }

    /// Creates a subscription for results of BaseIntent execution
    pub fn subscribe_for_results(
        &self,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
        self.commits_scheduler.subscribe_for_results()
    }

    /// Fetches current commit nonces
    pub async fn fetch_current_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        self.task_info_fetcher
            .fetch_current_commit_nonces(pubkeys, min_context_slot)
            .await
    }

    /// Dispatch worker
    #[instrument(skip(pending_result_listeners, results_subscription))]
    async fn dispatcher(
        mut results_subscription: broadcast::Receiver<
            BroadcastedIntentExecutionResult,
        >,
        pending_result_listeners: Arc<
            Mutex<HashMap<u64, BundleResultListener>>,
        >,
    ) {
        loop {
            let execution_result = match results_subscription.recv().await {
                Ok(result) => result,
                Err(broadcast::error::RecvError::Closed) => {
                    info!("Intent execution shutdown");
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // SAFETY: not really feasible to happen as this function is way faster than Intent execution
                    // requires investigation if ever happens!
                    error!(skipped, "Dispatcher lag detected");
                    continue;
                }
            };

            let sender = if let Some(sender) = pending_result_listeners
                .lock()
                .expect(POISONED_MUTEX_MSG)
                .remove(&execution_result.id)
            {
                sender
            } else {
                continue;
            };

            if let Err(execution_result) = sender.send(execution_result) {
                error!(
                    intent_id = execution_result.id,
                    "Failed to send execution result"
                );
            }
        }
    }
}
