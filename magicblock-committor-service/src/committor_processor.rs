use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{atomic::AtomicU64, Arc},
    time::{SystemTime, UNIX_EPOCH},
};

use magicblock_core::{
    intent::CommittedAccount, traits::ActionsCallbackScheduler,
};
use magicblock_program::magic_scheduled_base_intent::{
    CommitAndUndelegate, CommitType, MagicIntentBundle, ScheduledIntentBundle,
    UndelegateType,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::sync::broadcast;
use tracing::{error, info, instrument, warn};

use crate::{
    config::ChainConfig,
    error::CommittorServiceResult,
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
        CommitStatusRow, CommitType as PersistCommitType, IntentPersister,
        IntentPersisterImpl, MessageSignatures,
    },
};

const RECOVERY_MAX_AGE_SECS: u64 = 14 * 24 * 60 * 60;

pub(crate) struct CommittorProcessor {
    pub(crate) magicblock_rpc_client: MagicblockRpcClient,
    pub(crate) table_mania: TableMania,
    pub(crate) authority: Keypair,
    persister: IntentPersisterImpl,
    commits_scheduler: IntentExecutionManager<DummyDB>,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
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
        let websocket_uri = chain_config.websocket_uri.clone();
        let magic_block_rpc_client = match (chain_slot, websocket_uri) {
            (Some(chain_slot), websocket_uri) => {
                MagicblockRpcClient::new_with_chain_slot_and_websocket(
                    rpc_client,
                    chain_slot,
                    websocket_uri,
                )
            }
            (None, Some(websocket_uri)) => {
                MagicblockRpcClient::new_with_websocket(
                    rpc_client,
                    Some(websocket_uri),
                )
            }
            (None, None) => MagicblockRpcClient::new(rpc_client),
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

        Ok(Self {
            authority,
            magicblock_rpc_client: magic_block_rpc_client,
            table_mania,
            commits_scheduler,
            persister,
            task_info_fetcher,
        })
    }

    pub async fn active_lookup_tables(&self) -> Vec<Pubkey> {
        self.table_mania.active_table_addresses().await
    }

    pub async fn released_lookup_tables(&self) -> Vec<Pubkey> {
        self.table_mania.released_table_addresses().await
    }

    pub fn auth_pubkey(&self) -> Pubkey {
        self.authority.pubkey()
    }

    pub(crate) async fn reserve_pubkeys(
        &self,
        pubkeys: HashSet<Pubkey>,
    ) -> CommittorServiceResult<()> {
        Ok(self
            .table_mania
            .reserve_pubkeys(&self.authority, &pubkeys)
            .await?)
    }

    pub(crate) async fn release_pubkeys(&self, pubkeys: HashSet<Pubkey>) {
        self.table_mania.release_pubkeys(&pubkeys).await
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

    pub async fn pending_intent_bundles(
        &self,
    ) -> CommittorServiceResult<Vec<ScheduledIntentBundle>> {
        let recovery_time = unix_timestamp();
        let rows = self.persister.get_pending_commit_statuses(
            recovery_time.saturating_sub(RECOVERY_MAX_AGE_SECS),
        )?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let recovery_base_slot = self.magicblock_rpc_client.get_slot().await?;
        let bundles = pending_rows_to_scheduled_intent_bundles(
            rows,
            self.auth_pubkey(),
            recovery_base_slot,
            recovery_time,
        );
        if !bundles.is_empty() {
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

        Ok(bundles)
    }

    #[instrument(skip(self, intent_bundles))]
    pub async fn schedule_intent_bundle(
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
}

fn pending_rows_to_scheduled_intent_bundles(
    rows: Vec<CommitStatusRow>,
    payer: Pubkey,
    recovery_base_slot: u64,
    recovery_time: u64,
) -> Vec<ScheduledIntentBundle> {
    let mut grouped_rows = BTreeMap::<u64, Vec<CommitStatusRow>>::new();
    for row in rows {
        grouped_rows.entry(row.message_id).or_default().push(row);
    }

    grouped_rows
        .into_iter()
        .filter_map(|(message_id, rows)| {
            if rows.iter().any(|row| {
                !pending_row_is_in_recovery_window(row, recovery_time)
            })
            {
                warn!(
                    intent_id = message_id,
                    "Skipping pending commit intent outside recovery window"
                );
                return None;
            }

            let first = rows.first()?;
            let slot = first.slot;
            let blockhash = first.ephemeral_blockhash;
            if rows.iter().any(|r| {
                r.slot != slot || r.ephemeral_blockhash != blockhash
            }) {
                warn!(
                    intent_id = message_id,
                    "Skipping pending commit intent: rows disagree on slot or ephemeral_blockhash"
                );
                return None;
            }
            let mut commit_finalize_accounts = Vec::new();
            let mut commit_finalize_and_undelegate_accounts = Vec::new();

            for row in rows {
                let Some((account, undelegate)) =
                    committed_account_from_pending_row(row, recovery_base_slot)
                else {
                    continue;
                };
                if undelegate {
                    commit_finalize_and_undelegate_accounts.push(account);
                } else {
                    commit_finalize_accounts.push(account);
                }
            }

            let mut intent_bundle = MagicIntentBundle::default();
            if !commit_finalize_accounts.is_empty() {
                intent_bundle.commit_finalize =
                    Some(CommitType::Standalone(commit_finalize_accounts));
            }
            if !commit_finalize_and_undelegate_accounts.is_empty() {
                intent_bundle.commit_finalize_and_undelegate =
                    Some(CommitAndUndelegate {
                        commit_action: CommitType::Standalone(
                            commit_finalize_and_undelegate_accounts,
                        ),
                        undelegate_action: UndelegateType::Standalone,
                    });
            }
            if intent_bundle.is_empty() {
                return None;
            }

            Some(ScheduledIntentBundle {
                id: message_id,
                slot,
                blockhash,
                sent_transaction: Transaction::default(),
                payer,
                intent_bundle,
            })
        })
        .collect()
}

fn pending_row_is_in_recovery_window(
    row: &CommitStatusRow,
    recovery_time: u64,
) -> bool {
    recovery_time.saturating_sub(row.created_at) <= RECOVERY_MAX_AGE_SECS
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn committed_account_from_pending_row(
    row: CommitStatusRow,
    recovery_base_slot: u64,
) -> Option<(CommittedAccount, bool)> {
    if row.commit_type == PersistCommitType::DataAccount && row.data.is_none() {
        warn!(
            intent_id = row.message_id,
            pubkey = %row.pubkey,
            "Skipping pending data-account commit row without account data"
        );
        return None;
    }

    Some((
        CommittedAccount {
            pubkey: row.pubkey,
            account: Account {
                lamports: row.lamports,
                data: row.data.unwrap_or_default(),
                owner: row.delegated_account_owner,
                executable: false,
                rent_epoch: 0,
            },
            remote_slot: recovery_base_slot,
        },
        row.undelegate,
    ))
}

#[cfg(test)]
mod tests {
    use solana_hash::Hash;

    use super::*;
    use crate::persist::CommitStatus;

    fn pending_row(
        message_id: u64,
        pubkey: Pubkey,
        owner: Pubkey,
        blockhash: Hash,
        undelegate: bool,
        data: Option<Vec<u8>>,
    ) -> CommitStatusRow {
        let commit_type =
            if data.as_ref().map(|data| data.is_empty()) == Some(false) {
                PersistCommitType::DataAccount
            } else {
                PersistCommitType::EmptyAccount
            };

        CommitStatusRow {
            message_id,
            pubkey,
            commit_id: 0,
            delegated_account_owner: owner,
            slot: 42,
            ephemeral_blockhash: blockhash,
            undelegate,
            lamports: 1_000,
            data,
            commit_type,
            created_at: 1,
            commit_status: CommitStatus::Pending,
            commit_strategy: Default::default(),
            last_retried_at: 1,
            retries_count: 0,
        }
    }

    fn pending_row_with_timestamps(
        message_id: u64,
        created_at: u64,
        last_retried_at: u64,
    ) -> CommitStatusRow {
        let mut row = pending_row(
            message_id,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Hash::new_unique(),
            false,
            Some(vec![1]),
        );
        row.created_at = created_at;
        row.last_retried_at = last_retried_at;
        row
    }

    #[test]
    fn pending_rows_reconstruct_commit_finalize_bundle() {
        let payer = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let blockhash = Hash::new_unique();
        let commit_pubkey = Pubkey::new_unique();
        let undelegate_pubkey = Pubkey::new_unique();

        let bundles = pending_rows_to_scheduled_intent_bundles(
            vec![
                pending_row(
                    9,
                    commit_pubkey,
                    owner,
                    blockhash,
                    false,
                    Some(vec![1, 2, 3]),
                ),
                pending_row(9, undelegate_pubkey, owner, blockhash, true, None),
            ],
            payer,
            7,
            2,
        );

        assert_eq!(bundles.len(), 1);
        let bundle = &bundles[0];
        assert_eq!(bundle.id, 9);
        assert_eq!(bundle.slot, 42);
        assert_eq!(bundle.blockhash, blockhash);
        assert_eq!(bundle.payer, payer);

        let commit_accounts =
            bundle.get_commit_finalize_intent_accounts().unwrap();
        assert_eq!(commit_accounts.len(), 1);
        assert_eq!(commit_accounts[0].pubkey, commit_pubkey);
        assert_eq!(commit_accounts[0].account.data, vec![1, 2, 3]);
        assert_eq!(commit_accounts[0].remote_slot, 7);

        let undelegate_accounts = bundle
            .get_commit_finalize_and_undelegate_intent_accounts()
            .unwrap();
        assert_eq!(undelegate_accounts.len(), 1);
        assert_eq!(undelegate_accounts[0].pubkey, undelegate_pubkey);
        assert_eq!(undelegate_accounts[0].account.owner, owner);
        assert!(bundle.has_undelegate_intent());
    }

    #[test]
    fn pending_rows_skip_intents_older_than_recovery_window() {
        let payer = Pubkey::new_unique();
        let recovery_time = RECOVERY_MAX_AGE_SECS + 1;

        let recoverable = pending_rows_to_scheduled_intent_bundles(
            vec![pending_row_with_timestamps(
                1,
                recovery_time - RECOVERY_MAX_AGE_SECS,
                recovery_time,
            )],
            payer,
            7,
            recovery_time,
        );
        assert_eq!(recoverable.len(), 1);

        let recent = pending_rows_to_scheduled_intent_bundles(
            vec![pending_row_with_timestamps(2, recovery_time, recovery_time)],
            payer,
            7,
            recovery_time,
        );
        assert_eq!(recent.len(), 1);

        let too_old = pending_rows_to_scheduled_intent_bundles(
            vec![pending_row_with_timestamps(
                3,
                recovery_time - RECOVERY_MAX_AGE_SECS - 1,
                recovery_time - RECOVERY_MAX_AGE_SECS - 1,
            )],
            payer,
            7,
            recovery_time,
        );
        assert!(too_old.is_empty());
    }
}
