use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
};

use log::*;
use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use magicblock_rpc_client::{
    MagicBlockSendTransactionConfig, MagicblockRpcClient,
};
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    signature::{Keypair, Signature},
    signer::Signer,
};
use tokio::task::JoinSet;

use super::common::{lookup_table_keys, send_and_confirm};
use crate::{
    commit_scheduler::{db::DummyDB, CommitScheduler},
    compute_budget::{ComputeBudget, ComputeBudgetConfig},
    config::ChainConfig,
    error::CommittorServiceResult,
    persist::{
        CommitStatusRow, L1MessagePersister, L1MessagesPersisterIface,
        MessageSignatures,
    },
    types::{InstructionsForCommitable, InstructionsKind},
    CommitInfo,
};

pub(crate) struct CommittorProcessor {
    pub(crate) magicblock_rpc_client: MagicblockRpcClient,
    pub(crate) table_mania: TableMania,
    pub(crate) authority: Keypair,
    pub(crate) compute_budget_config: ComputeBudgetConfig,
    persister: L1MessagePersister,
    commits_scheduler: CommitScheduler<DummyDB>,
}

impl CommittorProcessor {
    pub fn try_new<P>(
        authority: Keypair,
        persist_file: P,
        chain_config: ChainConfig,
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
        let persister = L1MessagePersister::try_new(persist_file)?;

        // Create commit scheduler
        let commits_scheduler = CommitScheduler::new(
            magic_block_rpc_client.clone(),
            DummyDB::new(),
            persister.clone(),
            table_mania.clone(),
            chain_config.compute_budget_config.clone(),
        );

        Ok(Self {
            authority,
            magicblock_rpc_client: magic_block_rpc_client,
            table_mania,
            commits_scheduler,
            persister,
            compute_budget_config: chain_config.compute_budget_config,
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
            self.persister.get_commit_statuses_by_id(message_id)?;
        Ok(commit_statuses)
    }

    pub fn get_signature(
        &self,
        commit_id: u64,
    ) -> CommittorServiceResult<Option<MessageSignatures>> {
        let signatures = self.persister.get_signatures(commit_id)?;
        Ok(signatures)
    }

    pub async fn commit_l1_messages(
        &self,
        l1_messages: Vec<ScheduledL1Message>,
    ) {
        if let Err(err) = self.persister.start_l1_messages(&l1_messages) {
            // We will still try to perform the commits, but the fact that we cannot
            // persist the intent is very serious and we should probably restart the
            // valiator
            error!(
                "DB EXCEPTION: Failed to persist changeset to be committed: {:?}",
                err
            );
        };

        if let Err(err) = self.commits_scheduler.schedule(l1_messages).await {
            error!("Failed to schedule L1 message: {}", err);
            // TODO(edwin): handle
        }
    }

    pub(crate) async fn process_ixs_chunks(
        &self,
        ixs_chunks: Vec<Vec<InstructionsForCommitable>>,
        chunked_close_ixs: Option<Vec<Vec<InstructionsForCommitable>>>,
        table_mania: Option<&TableMania>,
        owners: &HashMap<Pubkey, Pubkey>,
    ) -> (
        Vec<(Signature, Vec<(CommitInfo, InstructionsKind)>)>,
        Vec<(Option<Signature>, Vec<CommitInfo>)>,
    ) {
        let latest_blockhash =
            match self.magicblock_rpc_client.get_latest_blockhash().await {
                Ok(bh) => bh,
                Err(err) => {
                    error!(
                    "Failed to get latest blockhash to process buffers: {:?}",
                    err
                );
                    // If we fail to get this blockhash we need to report all process
                    // instructions as failed
                    let commit_infos = ixs_chunks
                        .into_iter()
                        .map(|ixs_chunk| {
                            (
                                None::<Signature>,
                                ixs_chunk
                                    .into_iter()
                                    .map(|ixs| ixs.commit_info)
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .collect::<Vec<_>>();
                    return (vec![], commit_infos);
                }
            };

        let mut join_set = JoinSet::new();
        let successes = Arc::<
            Mutex<Vec<(Signature, Vec<(CommitInfo, InstructionsKind)>)>>,
        >::default();
        let failures =
            Arc::<Mutex<Vec<(Option<Signature>, Vec<CommitInfo>)>>>::default();
        for ixs_chunk in ixs_chunks {
            let authority = self.authority.insecure_clone();
            let rpc_client = self.magicblock_rpc_client.clone();
            let compute_budget =
                self.compute_budget_config.buffer_process_and_close_budget();
            let successes = successes.clone();
            let failures = failures.clone();
            let owners = owners.clone();
            let table_mania = table_mania.cloned();
            join_set.spawn(process_ixs_chunk(
                ixs_chunk,
                compute_budget,
                authority,
                rpc_client,
                successes,
                failures,
                table_mania,
                owners,
                latest_blockhash,
            ));
        }
        join_set.join_all().await;

        if let Some(chunked_close_ixs) = chunked_close_ixs {
            if log::log_enabled!(log::Level::Trace) {
                let ix_count =
                    chunked_close_ixs.iter().map(|x| x.len()).sum::<usize>();
                trace!(
                    "Processing {} close instruction chunk(s) with a total of {} instructions",
                    chunked_close_ixs.len(),
                    ix_count
                );
            }
            let latest_blockhash = match self
                .magicblock_rpc_client
                .get_latest_blockhash()
                .await
            {
                Ok(bh) => Some(bh),
                Err(err) => {
                    // If we fail to close the buffers then the commits were processed and we
                    // should not retry them, however eventually we'd want to close those buffers
                    error!(
                        "Failed to get latest blockhash to close buffer: {:?}",
                        err
                    );
                    let commit_infos = chunked_close_ixs
                        .iter()
                        .map(|ixs_chunk| {
                            ixs_chunk
                                .iter()
                                .map(|ixs| ixs.commit_info.clone())
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    error!("Therefore failed to close buffers for the following committed accounts: {:#?}", commit_infos);
                    None
                }
            };

            if let Some(latest_blockhash) = latest_blockhash {
                let mut join_set = JoinSet::new();
                let failures = Arc::<
                    Mutex<Vec<(Option<Signature>, Vec<CommitInfo>)>>,
                >::default();
                for ixs_chunk in chunked_close_ixs {
                    let authority = self.authority.insecure_clone();
                    let rpc_client = self.magicblock_rpc_client.clone();
                    let table_mania = table_mania.cloned();
                    let owners = owners.clone();
                    let compute_budget =
                        self.compute_budget_config.buffer_close_budget();
                    // We ignore close successes
                    let successes = Default::default();
                    // We only log close failures since the commit was processed successfully
                    let failures = failures.clone();
                    join_set.spawn(process_ixs_chunk(
                        ixs_chunk,
                        compute_budget,
                        authority,
                        rpc_client,
                        successes,
                        failures,
                        table_mania,
                        owners,
                        latest_blockhash,
                    ));
                }
                join_set.join_all().await;
                if !failures
                    .lock()
                    .expect("close failures mutex poisoned")
                    .is_empty()
                {
                    error!("Failed to to close some buffers: {:?}", failures);
                }
            }
        }

        let successes = Arc::try_unwrap(successes)
            .expect("successes mutex still has multiple owners")
            .into_inner()
            .expect("successes mutex was poisoned");
        let failures = Arc::try_unwrap(failures)
            .expect("failures mutex still has multiple owners")
            .into_inner()
            .expect("failures mutex was poisoned");

        (successes, failures)
    }
}

/// Processes a single chunk of instructions, sending them as a transaction.
/// Updates the shared success or failure lists based on the transaction outcome.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) async fn process_ixs_chunk(
    ixs_chunk: Vec<InstructionsForCommitable>,
    compute_budget: ComputeBudget,
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    successes: Arc<
        Mutex<Vec<(Signature, Vec<(CommitInfo, InstructionsKind)>)>>,
    >,
    failures: Arc<Mutex<Vec<(Option<Signature>, Vec<CommitInfo>)>>>,
    table_mania: Option<TableMania>,
    owners: HashMap<Pubkey, Pubkey>,
    latest_blockhash: Hash,
) {
    let mut ixs = vec![];
    let mut commit_infos = vec![];
    for ix_chunk in ixs_chunk.into_iter() {
        ixs.extend(ix_chunk.instructions);
        commit_infos.push((ix_chunk.commit_info, ix_chunk.kind));
    }
    let ixs_len = ixs.len();
    let table_mania_setup = table_mania.as_ref().map(|table_mania| {
        let committees = commit_infos
            .iter()
            .map(|(x, _)| x.pubkey())
            .collect::<HashSet<_>>();
        let keys_from_table =
            lookup_table_keys(&authority, &committees, &owners);
        (table_mania, keys_from_table)
    });
    let compute_budget_ixs = compute_budget.instructions(commit_infos.len());
    match send_and_confirm(
        rpc_client,
        &authority,
        [compute_budget_ixs, ixs].concat(),
        "process commitable and/or close pdas".to_string(),
        Some(latest_blockhash),
        MagicBlockSendTransactionConfig::ensure_committed(),
        table_mania_setup,
    )
    .await
    {
        Ok(sig) => {
            successes
                .lock()
                .expect("ix successes mutex poisoned")
                .push((sig, commit_infos));
        }
        Err(err) => {
            error!(
                "Processing {} instructions for {} commit infos {:?}",
                ixs_len,
                commit_infos.len(),
                err
            );
            let commit_infos = commit_infos
                .into_iter()
                .map(|(commit_info, _)| commit_info)
                .collect();
            failures
                .lock()
                .expect("ix failures mutex poisoned")
                .push((err.signature(), commit_infos));
        }
    }
}
