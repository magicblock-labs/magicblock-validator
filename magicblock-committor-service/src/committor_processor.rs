use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use light_client::indexer::photon_indexer::PhotonIndexer;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use tokio::sync::broadcast;
use tracing::{error, instrument};

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

pub(crate) struct CommittorProcessor {
    pub(crate) magicblock_rpc_client: MagicblockRpcClient,
    pub(crate) table_mania: TableMania,
    pub(crate) authority: Keypair,
    persister: IntentPersisterImpl,
    commits_scheduler: IntentExecutionManager<DummyDB>,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    compression_enabled: bool,
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
        let photon_client = chain_config
            .photon_uri
            .as_ref()
            .map(|uri| Arc::new(PhotonIndexer::new(uri.to_string())));
        let compression_enabled = photon_client.is_some();

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
        let task_info_fetcher =
            Arc::new(CacheTaskInfoFetcher::new(RpcTaskInfoFetcher::new(
                magic_block_rpc_client.clone(),
                photon_client,
            )));
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
            compression_enabled,
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

    #[instrument(skip(self, intent_bundles))]
    pub async fn schedule_intent_bundle(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> CommittorServiceResult<()> {
        if !self.compression_enabled
            && intent_bundles
                .iter()
                .any(ScheduledIntentBundle::has_compressed_intent)
        {
            return Err(CommittorServiceError::CompressionNotConfigured);
        }

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
        compressed: bool,
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        self.task_info_fetcher
            .fetch_current_commit_nonces(pubkeys, compressed, min_context_slot)
            .await
    }
}

#[cfg(test)]
mod tests {
    use magicblock_core::{
        intent::BaseActionCallback,
        traits::{
            ActionResult, ActionsCallbackScheduler, CallbackScheduleError,
        },
    };
    use solana_hash::Hash;
    use solana_signature::Signature;
    use solana_transaction::Transaction;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::{
        compute_budget::ComputeBudgetConfig,
        config::{ChainConfig, DEFAULT_ACTIONS_TIMEOUT},
        error::CommittorServiceError,
        intent_execution_manager::intent_scheduler::create_test_compressed_intent,
    };

    #[derive(Clone, Default)]
    struct NoopActionsCallbackScheduler;

    impl ActionsCallbackScheduler for NoopActionsCallbackScheduler {
        fn schedule(
            &self,
            callbacks: Vec<BaseActionCallback>,
            _signature: Option<Signature>,
            _result: ActionResult,
        ) -> Vec<Result<Signature, CallbackScheduleError>> {
            callbacks
                .iter()
                .map(|_| Ok(Signature::new_unique()))
                .collect()
        }
    }

    #[tokio::test]
    async fn compressed_intents_fail_without_photon_indexer() {
        let persist_file = NamedTempFile::new().unwrap();
        let processor = CommittorProcessor::try_new(
            Keypair::new(),
            persist_file.path(),
            ChainConfig {
                rpc_uri: "http://localhost:7799".to_string(),
                photon_uri: None,
                commitment:
                    solana_commitment_config::CommitmentConfig::processed(),
                compute_budget_config: ComputeBudgetConfig::new(1_000_000),
                actions_timeout: DEFAULT_ACTIONS_TIMEOUT,
            },
            NoopActionsCallbackScheduler,
        )
        .unwrap();

        let mut intent =
            create_test_compressed_intent(1, &[Pubkey::new_unique()], false);
        intent.blockhash = Hash::new_unique();
        intent.sent_transaction = Transaction::default();

        let result = processor.schedule_intent_bundle(vec![intent]).await;

        assert!(matches!(
            result,
            Err(CommittorServiceError::CompressionNotConfigured)
        ));
    }
}
