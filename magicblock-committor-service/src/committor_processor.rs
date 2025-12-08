use std::{collections::HashSet, path::Path, sync::Arc};

use light_client::indexer::photon_indexer::PhotonIndexer;
use log::*;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, signature::Keypair, signer::Signer,
};
use tokio::sync::broadcast;

use crate::{
    config::ChainConfig,
    error::CommittorServiceResult,
    intent_execution_manager::{
        db::DummyDB, BroadcastedIntentExecutionResult, IntentExecutionManager,
    },
    persist::{
        CommitStatusRow, IntentPersister, IntentPersisterImpl,
        MessageSignatures,
    },
    types::ScheduledBaseIntentWrapper,
};

pub(crate) struct CommittorProcessor {
    pub(crate) magicblock_rpc_client: MagicblockRpcClient,
    pub(crate) table_mania: TableMania,
    pub(crate) authority: Keypair,
    persister: IntentPersisterImpl,
    commits_scheduler: IntentExecutionManager<DummyDB>,
}

impl CommittorProcessor {
    pub fn try_new<P>(
        authority: Keypair,
        persist_file: P,
        chain_config: ChainConfig,
        photon_client: Option<Arc<PhotonIndexer>>,
    ) -> CommittorServiceResult<Self>
    where
        P: AsRef<Path>,
    {
        let rpc_client = RpcClient::new_with_commitment(
            chain_config.rpc_uri.to_string(),
            CommitmentConfig {
                commitment: chain_config.commitment,
            },
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
        let commits_scheduler = IntentExecutionManager::new(
            magic_block_rpc_client.clone(),
            photon_client.clone(),
            DummyDB::new(),
            Some(persister.clone()),
            table_mania.clone(),
            chain_config.compute_budget_config.clone(),
        );

        Ok(Self {
            authority,
            magicblock_rpc_client: magic_block_rpc_client,
            table_mania,
            commits_scheduler,
            persister,
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

    pub async fn schedule_base_intents(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> CommittorServiceResult<()> {
        let intents = base_intents
            .iter()
            .map(|base_intent| base_intent.inner.clone())
            .collect::<Vec<_>>();
        if let Err(err) = self.persister.start_base_intents(&intents) {
            // We will still try to perform the commits, but the fact that we cannot
            // persist the intent is very serious and we should probably restart the
            // valiator
            error!(
                "DB EXCEPTION: Failed to persist changeset to be committed: {:?}",
                err
            );
        };

        self.commits_scheduler
            .schedule(base_intents)
            .await
            .inspect_err(|err| {
                error!("Failed to schedule base intent: {}", err);
            })?;

        Ok(())
    }

    /// Creates a subscription for results of BaseIntent execution
    pub fn subscribe_for_results(
        &self,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
        self.commits_scheduler.subscribe_for_results()
    }
}
