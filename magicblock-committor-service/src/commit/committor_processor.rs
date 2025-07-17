use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
};

use log::*;
use magicblock_committor_program::{Changeset, ChangesetMeta};
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
    commit_stage::CommitStage,
    commit_strategy::{split_changesets_by_commit_strategy, SplitChangesets},
    compute_budget::{ComputeBudget, ComputeBudgetConfig},
    config::ChainConfig,
    error::CommittorServiceResult,
    persist::{
        BundleSignatureRow, CommitPersister, CommitStatusRow, CommitStrategy,
    },
    pubkeys_provider::provide_committee_pubkeys,
    types::{InstructionsForCommitable, InstructionsKind},
    CommitInfo,
};

pub(crate) struct CommittorProcessor {
    pub(crate) magicblock_rpc_client: MagicblockRpcClient,
    pub(crate) table_mania: TableMania,
    pub(crate) authority: Keypair,
    pub(crate) persister: Arc<Mutex<CommitPersister>>,
    pub(crate) compute_budget_config: Arc<ComputeBudgetConfig>,
}

impl Clone for CommittorProcessor {
    fn clone(&self) -> Self {
        Self {
            magicblock_rpc_client: self.magicblock_rpc_client.clone(),
            table_mania: self.table_mania.clone(),
            authority: self.authority.insecure_clone(),
            persister: self.persister.clone(),
            compute_budget_config: self.compute_budget_config.clone(),
        }
    }
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
        let gc_config = GarbageCollectorConfig::default();
        let table_mania = TableMania::new(
            magic_block_rpc_client.clone(),
            &authority,
            Some(gc_config),
        );
        let persister = CommitPersister::try_new(persist_file)?;
        Ok(Self {
            authority,
            magicblock_rpc_client: magic_block_rpc_client,
            table_mania,
            persister: Arc::new(Mutex::new(persister)),
            compute_budget_config: Arc::new(chain_config.compute_budget_config),
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
        reqid: &str,
    ) -> CommittorServiceResult<Vec<CommitStatusRow>> {
        let commit_statuses = self
            .persister
            .lock()
            .expect("persister mutex poisoned")
            .get_commit_statuses_by_reqid(reqid)?;
        Ok(commit_statuses)
    }

    pub fn get_signature(
        &self,
        bundle_id: u64,
    ) -> CommittorServiceResult<Option<BundleSignatureRow>> {
        let signatures = self
            .persister
            .lock()
            .expect("persister mutex poisoned")
            .get_signature(bundle_id)?;
        Ok(signatures)
    }

    pub async fn commit_changeset(
        &self,
        changeset: Changeset,
        finalize: bool,
        ephemeral_blockhash: Hash,
    ) -> Option<String> {
        let reqid = match self
            .persister
            .lock()
            .expect("persister mutex poisoned")
            .start_changeset(&changeset, ephemeral_blockhash, finalize)
        {
            Ok(id) => Some(id),
            Err(err) => {
                // We will still try to perform the commits, but the fact that we cannot
                // persist the intent is very serious and we should probably restart the
                // valiator
                error!(
                    "DB EXCEPTION: Failed to persist changeset to be committed: {:?}",
                    err
                );
                None
            }
        };
        let owners = changeset.owners();
        let commit_stages = self
            .process_commit_changeset(changeset, finalize, ephemeral_blockhash)
            .await;

        // Release pubkeys related to all undelegated accounts from the lookup tables
        let releaseable_pubkeys = commit_stages
            .iter()
            .filter(|x| CommitStage::is_successfully_undelegated(x))
            .flat_map(|x| {
                provide_committee_pubkeys(&x.pubkey(), owners.get(&x.pubkey()))
            })
            .collect::<HashSet<_>>();
        self.table_mania.release_pubkeys(&releaseable_pubkeys).await;

        if let Some(reqid) = &reqid {
            for stage in commit_stages {
                let _ = self.persister
                    .lock()
                    .expect("persister mutex poisoned")
                    .update_status(
                        reqid,
                        &stage.pubkey(),
                        stage.commit_status(),
                    ).map_err(|err| {
                        // We log the error here, but there is nothing we can do if we encounter
                        // a db issue.
                        error!(
                            "DB EXCEPTION: Failed to update status of changeset {}: {:?}",
                            reqid, err
                        );
                    });
            }
        }

        reqid
    }

    async fn process_commit_changeset(
        &self,
        changeset: Changeset,
        finalize: bool,
        ephemeral_blockhash: Hash,
    ) -> Vec<CommitStage> {
        let changeset_meta = ChangesetMeta::from(&changeset);
        let SplitChangesets {
            args_changesets,
            args_including_finalize_changesets,
            args_with_lookup_changesets,
            args_including_finalize_with_lookup_changesets,
            from_buffer_changesets,
            from_buffer_with_lookup_changesets,
        } = match split_changesets_by_commit_strategy(changeset, finalize) {
            Ok(changesets) => changesets,
            Err(err) => {
                error!("Failed to split changesets: {:?}", err);
                return changeset_meta
                    .into_account_infos()
                    .into_iter()
                    .map(CommitStage::SplittingChangesets)
                    .collect();
            }
        };

        debug_assert!(
            finalize
                || (args_including_finalize_changesets.is_empty()
                    && args_including_finalize_with_lookup_changesets
                        .is_empty()),
            "BUG: args including finalize strategies should not be created when not finalizing"
        );

        let mut join_set = JoinSet::new();
        if !args_changesets.is_empty()
            || !args_with_lookup_changesets.is_empty()
            || !args_including_finalize_changesets.is_empty()
            || !args_including_finalize_with_lookup_changesets.is_empty()
        {
            let latest_blockhash = match self
                .magicblock_rpc_client
                .get_latest_blockhash()
                .await
            {
                Ok(bh) => bh,
                Err(err) => {
                    error!(
                            "Failed to get latest blockhash to commit using args: {:?}",
                            err
                        );
                    let strategy = CommitStrategy::args(
                        !args_with_lookup_changesets.is_empty()
                            || !args_including_finalize_with_lookup_changesets
                                .is_empty(),
                    );
                    return changeset_meta
                        .into_account_infos()
                        .into_iter()
                        .map(|(meta, slot, undelegate)| {
                            CommitStage::GettingLatestBlockhash((
                                meta, slot, undelegate, strategy,
                            ))
                        })
                        .collect();
                }
            };

            for changeset in args_changesets {
                join_set.spawn(Self::commit_changeset_using_args(
                    Arc::new(self.clone()),
                    changeset,
                    (finalize, true),
                    ephemeral_blockhash,
                    latest_blockhash,
                    false,
                ));
            }

            for changeset in args_including_finalize_changesets {
                join_set.spawn(Self::commit_changeset_using_args(
                    Arc::new(self.clone()),
                    changeset,
                    (finalize, false),
                    ephemeral_blockhash,
                    latest_blockhash,
                    false,
                ));
            }

            for changeset in args_with_lookup_changesets {
                join_set.spawn(Self::commit_changeset_using_args(
                    Arc::new(self.clone()),
                    changeset,
                    (finalize, true),
                    ephemeral_blockhash,
                    latest_blockhash,
                    true,
                ));
            }

            for changeset in args_including_finalize_with_lookup_changesets {
                join_set.spawn(Self::commit_changeset_using_args(
                    Arc::new(self.clone()),
                    changeset,
                    (finalize, false),
                    ephemeral_blockhash,
                    latest_blockhash,
                    true,
                ));
            }
        }

        for changeset in from_buffer_changesets {
            join_set.spawn(Self::commit_changeset_using_buffers(
                Arc::new(self.clone()),
                changeset,
                finalize,
                ephemeral_blockhash,
                false,
            ));
        }
        for changeset in from_buffer_with_lookup_changesets {
            join_set.spawn(Self::commit_changeset_using_buffers(
                Arc::new(self.clone()),
                changeset,
                finalize,
                ephemeral_blockhash,
                true,
            ));
        }

        join_set.join_all().await.into_iter().flatten().collect()
    }

    pub(crate) async fn process_ixs_chunks(
        &self,
        ixs_chunks: Vec<Vec<InstructionsForCommitable>>,
        chunked_close_ixs: Option<Vec<Vec<InstructionsForCommitable>>>,
        table_mania: Option<&TableMania>,
        owners: &HashMap<Pubkey, Pubkey>,
    ) -> (ProcessIxChunkSuccesses, ProcessIxChunkFailures) {
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
        let successes = ProcessIxChunkSuccessesRc::default();
        let failures = ProcessIxChunkFailuresRc::default();
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
                let failures = ProcessIxChunkFailuresRc::default();
                for ixs_chunk in chunked_close_ixs {
                    let authority = self.authority.insecure_clone();
                    let rpc_client = self.magicblock_rpc_client.clone();
                    let table_mania = table_mania.cloned();
                    let owners = owners.clone();
                    let compute_budget =
                        self.compute_budget_config.buffer_close_budget();
                    // We ignore close successes
                    let successes = ProcessIxChunkSuccessesRc::default();
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

pub(crate) type ProcessIxChunkSuccesses =
    Vec<(Signature, Vec<(CommitInfo, InstructionsKind)>)>;
pub(crate) type ProcessIxChunkSuccessesRc = Arc<Mutex<ProcessIxChunkSuccesses>>;
pub(crate) type ProcessIxChunkFailures =
    Vec<(Option<Signature>, Vec<CommitInfo>)>;
pub(crate) type ProcessIxChunkFailuresRc = Arc<Mutex<ProcessIxChunkFailures>>;

/// Processes a single chunk of instructions, sending them as a transaction.
/// Updates the shared success or failure lists based on the transaction outcome.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) async fn process_ixs_chunk(
    ixs_chunk: Vec<InstructionsForCommitable>,
    compute_budget: ComputeBudget,
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    successes: ProcessIxChunkSuccessesRc,
    failures: ProcessIxChunkFailuresRc,
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
        authority,
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
