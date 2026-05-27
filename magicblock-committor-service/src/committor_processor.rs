use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::Arc,
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
        let magic_block_rpc_client = MagicblockRpcClient::new(rpc_client);

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
        let rows = self.persister.get_pending_commit_statuses()?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let recovery_base_slot = self.magicblock_rpc_client.get_slot().await?;
        let bundles = pending_rows_to_scheduled_intent_bundles(
            rows,
            self.auth_pubkey(),
            recovery_base_slot,
        );
        if !bundles.is_empty() {
            let accounts_count: usize = bundles
                .iter()
                .map(|bundle| bundle.get_all_committed_pubkeys().len())
                .sum();
            info!(
                intent_count = bundles.len(),
                accounts_count,
                "Recovered pending commit intents from persistence"
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
) -> Vec<ScheduledIntentBundle> {
    let mut grouped_rows = BTreeMap::<u64, Vec<CommitStatusRow>>::new();
    for row in rows {
        grouped_rows.entry(row.message_id).or_default().push(row);
    }

    grouped_rows
        .into_iter()
        .filter_map(|(message_id, rows)| {
            let first = rows.first()?;
            let slot = first.slot;
            let blockhash = first.ephemeral_blockhash;
            let mut commit_finalize_accounts = Vec::new();
            let mut commit_finalize_and_undelegate_accounts = Vec::new();

            for row in rows {
                let Some(account) =
                    committed_account_from_pending_row(row, recovery_base_slot)
                else {
                    continue;
                };
                if account.1 {
                    commit_finalize_and_undelegate_accounts.push(account.0);
                } else {
                    commit_finalize_accounts.push(account.0);
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
}
