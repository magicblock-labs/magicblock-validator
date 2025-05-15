use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use borsh::{to_vec, BorshDeserialize};
use dlp::pda::commit_state_pda_from_delegated_account;
use log::*;
use magicblock_committor_program::{
    instruction::{
        create_init_ix, create_realloc_buffer_ixs,
        create_realloc_buffer_ixs_to_add_remaining, create_write_ix,
        CreateInitIxArgs, CreateReallocBufferIxArgs, CreateWriteIxArgs,
    },
    instruction_chunks::chunk_realloc_ixs,
    Changeset, ChangesetChunk, Chunks, CommitableAccount,
};
use magicblock_rpc_client::{
    MagicBlockRpcClientError, MagicBlockRpcClientResult,
    MagicBlockSendTransactionConfig,
};
use solana_pubkey::Pubkey;
use solana_sdk::{hash::Hash, instruction::Instruction, signer::Signer};
use tokio::task::JoinSet;

use super::{
    common::send_and_confirm,
    process_buffers::{
        chunked_ixs_to_process_commitables_and_close_pdas,
        ChunkedIxsToProcessCommitablesAndClosePdasResult,
    },
    CommittorProcessor,
};
use crate::{
    commit::common::get_accounts_to_undelegate,
    commit_stage::CommitSignatures,
    error::{CommitAccountError, CommitAccountResult},
    finalize::{
        chunked_ixs_to_finalize_commitables,
        ChunkedIxsToFinalizeCommitablesResult,
    },
    persist::CommitStrategy,
    types::InstructionsKind,
    undelegate::{
        chunked_ixs_to_undelegate_commitables, undelegate_commitables_ixs,
    },
    CommitInfo, CommitStage,
};

struct NextReallocs {
    missing_size: u64,
    start_idx: usize,
}

impl CommittorProcessor {
    /// Commits the changeset by initializing the accounts, writing the chunks,
    /// and closing the pdas.
    /// NOTE: we return no error since the validator would not know how to mitigate
    /// the problem.
    pub async fn commit_changeset_using_buffers(
        processor: Arc<Self>,
        changeset: Changeset,
        finalize: bool,
        ephemeral_blockhash: Hash,
        use_lookup: bool,
    ) -> Vec<CommitStage> {
        macro_rules! handle_unchunked {
            ($unchunked:ident, $commit_stages:ident, $commit_stage:expr) => {
                for (bundle_id, commit_infos) in $unchunked.into_iter() {
                    // The max amount of accounts we can commit and process as part of a single
                    // transaction is [crate::max_per_transaction::MAX_COMMIT_STATE_AND_CLOSE_PER_TRANSACTION].
                    warn!(
                        "Commit infos for bundle id {} are too many to be processed in a single transaction",
                        bundle_id
                    );
                    $commit_stages.extend(
                        commit_infos
                            .into_iter()
                            .map($commit_stage),
                    );
                }
            }
        }

        let owners = changeset.owners();
        let accounts_len = changeset.account_keys().len();
        let commit_strategy = if use_lookup {
            CommitStrategy::FromBufferWithLookupTable
        } else {
            CommitStrategy::FromBuffer
        };
        let accounts_to_undelegate =
            get_accounts_to_undelegate(&changeset, finalize);
        let results = processor
            .prepare_changeset_buffers(
                changeset,
                ephemeral_blockhash,
                commit_strategy,
                finalize,
            )
            .await;

        let mut commit_stages = vec![];

        // 1. Init Buffer and Chunks Account
        let (mut succeeded_inits, failed_inits): (Vec<_>, Vec<_>) = {
            let (succeeded, failed): (Vec<_>, Vec<_>) =
                results.into_iter().partition(Result::is_ok);
            (
                succeeded
                    .into_iter()
                    .map(Result::unwrap)
                    .collect::<Vec<_>>(),
                failed
                    .into_iter()
                    .map(Result::unwrap_err)
                    .collect::<Vec<_>>(),
            )
        };

        // If we couldn't init the buffers for a specific commit then we're done with it.
        for commit_err in failed_inits.into_iter() {
            let commit_stage = CommitStage::from(commit_err);
            let bundle_id = commit_stage.commit_metadata().bundle_id();
            commit_stages.push(commit_stage);

            // We also need to skip all committables that are in the same bundle as
            // a commit we're giving up on.
            let (fail_in_order_to_respect_bundle, keep): (Vec<_>, Vec<_>) =
                succeeded_inits.drain(..).partition(|commit_info| {
                    #[allow(clippy::let_and_return)]
                    let same_bundle = commit_info.bundle_id() == bundle_id;
                    same_bundle
                });
            commit_stages.extend(
                fail_in_order_to_respect_bundle.into_iter().map(|x| {
                    CommitStage::BufferAndChunkFullyInitialized((
                        x,
                        commit_strategy,
                    ))
                }),
            );
            succeeded_inits.extend(keep);
        }

        // 2. Create chunks of instructions that process the commits and respect desired bundles
        let ChunkedIxsToProcessCommitablesAndClosePdasResult {
            chunked_ixs,
            chunked_close_ixs,
            unchunked,
        } = chunked_ixs_to_process_commitables_and_close_pdas(
            processor.authority.pubkey(),
            succeeded_inits.clone(),
            use_lookup,
        );
        handle_unchunked!(
            unchunked,
            commit_stages,
            CommitStage::PartOfTooLargeBundleToProcess
        );

        // 3. Process all chunks via transactions, one per chunk of instructions
        trace!(
            "ChunkedIxs: {}",
            chunked_ixs
                .iter()
                .map(|xs| xs
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"))
                .collect::<Vec<_>>()
                .join("]\n\n[\n")
        );
        debug_assert_eq!(
            chunked_ixs.iter().map(|x| x.len()).sum::<usize>() + commit_stages.len(),
            accounts_len,
            "Sum of instructions and early bail out stages should have one instruction per commmitted account",
        );

        let table_mania = use_lookup.then(|| processor.table_mania.clone());
        let (succeeded_process, failed_process) = processor
            .process_ixs_chunks(
                chunked_ixs,
                chunked_close_ixs,
                table_mania.as_ref(),
                &owners,
            )
            .await;

        commit_stages.extend(failed_process.into_iter().flat_map(
            |(sig, xs)| {
                let sigs = sig.map(|x| CommitSignatures {
                    process_signature: x,
                    finalize_signature: None,
                    undelegate_signature: None,
                });
                xs.into_iter()
                    .map(|x| {
                        CommitStage::FailedProcess((
                            x,
                            commit_strategy,
                            sigs.as_ref().cloned(),
                        ))
                    })
                    .collect::<Vec<_>>()
            },
        ));

        let mut processed_commit_infos = vec![];
        let mut processed_signatures = HashMap::new();
        for (sig, commit_infos) in succeeded_process {
            if log_enabled!(Level::Trace) {
                let kinds = commit_infos
                    .iter()
                    .map(|(_, kind)| *kind)
                    .collect::<HashSet<InstructionsKind>>();
                let handled = kinds
                    .iter()
                    .map(|x| format!("{:?}", x))
                    .collect::<Vec<_>>()
                    .join(" | ");
                trace!(
                    "Successfully handled ({}) for {} commit info(s) via transaction '{}'",
                    handled,
                    commit_infos.len(),
                    sig
                );
            }
            for (commit_info, _) in commit_infos
                .into_iter()
                .filter(|(_, kind)| kind.is_processing())
            {
                let bundle_id = commit_info.bundle_id();
                debug_assert!(
                processed_signatures
                    .get(&bundle_id)
                    .map(|x| x == &sig)
                    .unwrap_or(true),
                "BUG: Same processed bundle ids should have the same signature"
            );
                processed_signatures.insert(bundle_id, sig);
                processed_commit_infos.push(commit_info);
            }
        }

        // 4. Optionally finalize + undelegate all processed commits also respecting bundles
        if finalize && !processed_commit_infos.is_empty() {
            // 4.1. Create chunks of finalize instructions that fit in a single transaction
            let ChunkedIxsToFinalizeCommitablesResult {
                chunked_ixs,
                unchunked,
            } = chunked_ixs_to_finalize_commitables(
                processor.authority.pubkey(),
                processed_commit_infos,
                use_lookup,
            );
            handle_unchunked!(
                unchunked,
                commit_stages,
                CommitStage::PartOfTooLargeBundleToFinalize
            );

            // 4.2. Run each finalize chunk in a single transaction
            let (succeeded_finalize, failed_finalize): (Vec<_>, Vec<_>) =
                processor
                    .process_ixs_chunks(
                        chunked_ixs,
                        None,
                        table_mania.as_ref(),
                        &owners,
                    )
                    .await;
            commit_stages.extend(failed_finalize.into_iter().flat_map(
                |(sig, infos)| {
                    infos
                        .into_iter()
                        .map(|x| {
                            let bundle_id = x.bundle_id();
                            CommitStage::FailedFinalize((
                                x,
                                commit_strategy,
                                CommitSignatures {
                                    // SAFETY: signatures for all bundles of succeeded process transactions
                                    //         have been added above
                                    process_signature: *processed_signatures
                                        .get(&bundle_id)
                                        .unwrap(),
                                    finalize_signature: sig,
                                    undelegate_signature: None,
                                },
                            ))
                        })
                        .collect::<Vec<_>>()
                },
            ));

            let mut finalized_commit_infos = vec![];
            let mut finalized_signatures = HashMap::new();
            for (sig, commit_infos) in succeeded_finalize {
                trace!(
                "Successfully finalized {} commit infos via transaction '{}'",
                commit_infos.len(),
                sig
            );
                for (commit_info, kind) in commit_infos.iter() {
                    debug_assert_eq!(
                        kind,
                        &InstructionsKind::Finalize,
                        "Expecting separate finalize instructions onky"
                    );
                    let bundle_id = commit_info.bundle_id();
                    debug_assert!(
                        finalized_signatures
                            .get(&bundle_id)
                            .map(|x| x == &sig)
                            .unwrap_or(true),
                        "BUG: Same finalized bundle ids should have the same signature"
                    );

                    finalized_signatures.insert(bundle_id, sig);
                }
                let commit_infos = commit_infos
                    .into_iter()
                    .map(|(info, _)| info)
                    .collect::<Vec<_>>();
                finalized_commit_infos.extend(commit_infos);
            }
            // 4.2. Consider undelegation by first dividing finalized accounts into two sets,
            let (finalize_and_undelegate, finalize_only) =
                finalized_commit_infos
                    .into_iter()
                    .partition::<Vec<_>, _>(|x| x.undelegate());
            // 4.3.a accounts we don't need to undelegate are done
            commit_stages.extend(finalize_only.into_iter().map(|x| {
                let bundle_id = x.bundle_id();
                CommitStage::Succeeded((
                    x,
                    commit_strategy,
                    CommitSignatures {
                        // SAFETY: signatures for all bundles of succeeded process transactions
                        //         have been added above
                        process_signature: *processed_signatures
                            .get(&bundle_id)
                            .unwrap(),
                        finalize_signature: finalized_signatures
                            .get(&bundle_id)
                            .cloned(),
                        undelegate_signature: None,
                    },
                ))
            }));
            // 4.3.b the other accounts need to be undelegated first
            if let Some(accounts) = accounts_to_undelegate {
                debug_assert_eq!(
                    accounts.len(),
                    finalize_and_undelegate.len(),
                    "BUG: same amount of accounts to undelegate as to finalize and undelegate"
                );
                let undelegate_ixs = match undelegate_commitables_ixs(
                    &processor.magicblock_rpc_client,
                    processor.authority.pubkey(),
                    accounts,
                )
                .await
                {
                    Ok(ixs) => Some(ixs),
                    Err(err) => {
                        error!(
                        "Failed to prepare accounts undelegation '{}': {:?}",
                        err, err
                    );
                        commit_stages.extend(
                            finalize_and_undelegate.iter().map(|x| {
                                let bundle_id = x.bundle_id();
                                CommitStage::FailedUndelegate((
                                    x.clone(),
                                    CommitStrategy::args(use_lookup),
                                    CommitSignatures {
                                        // SAFETY: signatures for all bundles of succeeded process transactions
                                        //         have been added above
                                        process_signature:
                                            *processed_signatures
                                                .get(&bundle_id)
                                                .unwrap(),
                                        finalize_signature:
                                            finalized_signatures
                                                .get(&bundle_id)
                                                .cloned(),
                                        undelegate_signature: err.signature(),
                                    },
                                ))
                            }),
                        );
                        None
                    }
                };
                if let Some(undelegate_ixs) = undelegate_ixs {
                    let chunked_ixs = chunked_ixs_to_undelegate_commitables(
                        undelegate_ixs,
                        finalize_and_undelegate,
                        use_lookup,
                    );
                    let (succeeded_undelegate, failed_undelegate): (
                        Vec<_>,
                        Vec<_>,
                    ) = processor
                        .process_ixs_chunks(
                            chunked_ixs,
                            None,
                            table_mania.as_ref(),
                            &owners,
                        )
                        .await;

                    commit_stages.extend(
                        failed_undelegate.into_iter().flat_map(
                            |(sig, infos)| {
                                infos
                                    .into_iter()
                                    .map(|x| {
                                        let bundle_id = x.bundle_id();
                                        CommitStage::FailedUndelegate((
                                            x,
                                            commit_strategy,
                                            CommitSignatures {
                                                // SAFETY: signatures for all bundles of succeeded process transactions
                                                //         have been added above
                                                process_signature:
                                                    *processed_signatures
                                                        .get(&bundle_id)
                                                        .unwrap(),
                                                finalize_signature:
                                                    finalized_signatures
                                                        .get(&bundle_id)
                                                        .cloned(),
                                                undelegate_signature: sig,
                                            },
                                        ))
                                    })
                                    .collect::<Vec<_>>()
                            },
                        ),
                    );
                    commit_stages.extend(
                        succeeded_undelegate.into_iter().flat_map(
                            |(sig, infos)| {
                                infos
                                    .into_iter()
                                    .map(|(x, _)| {
                                        let bundle_id = x.bundle_id();
                                        CommitStage::Succeeded((
                                            x,
                                            commit_strategy,
                                            CommitSignatures {
                                                // SAFETY: signatures for all bundles of succeeded process transactions
                                                //         have been added above
                                                process_signature:
                                                    *processed_signatures
                                                        .get(&bundle_id)
                                                        .unwrap(),
                                                finalize_signature:
                                                    finalized_signatures
                                                        .get(&bundle_id)
                                                        .cloned(),
                                                undelegate_signature: Some(sig),
                                            },
                                        ))
                                    })
                                    .collect::<Vec<_>>()
                            },
                        ),
                    );
                }
            } else {
                debug_assert!(
                    finalize_and_undelegate.is_empty(),
                    "BUG: We should either have accounts to undelegate or an empty finalize_and_undelegate"
                );
            }
        } else {
            commit_stages.extend(processed_commit_infos.into_iter().map(|x| {
                let bundle_id = x.bundle_id();
                CommitStage::Succeeded((
                    x,
                    commit_strategy,
                    CommitSignatures {
                        // SAFETY: signatures for all bundles of succeeded process transactions
                        //         have been added above
                        process_signature: *processed_signatures
                            .get(&bundle_id)
                            .unwrap(),
                        finalize_signature: None,
                        undelegate_signature: None,
                    },
                ))
            }));
        }

        debug_assert_eq!(
            accounts_len,
            CommitStage::commit_infos(&commit_stages).len(),
            "Should have one commit stage per commmitted account ({}) {:#?}",
            accounts_len,
            commit_stages
        );

        commit_stages
    }

    async fn prepare_changeset_buffers(
        &self,
        changeset: Changeset,
        ephemeral_blockhash: Hash,
        commit_strategy: CommitStrategy,
        finalize: bool,
    ) -> Vec<CommitAccountResult<CommitInfo>> {
        let commitables =
            changeset.into_committables(crate::consts::MAX_WRITE_CHUNK_SIZE);
        let mut join_set: JoinSet<CommitAccountResult<CommitInfo>> =
            JoinSet::new();
        for commitable in commitables {
            let me = Arc::new(self.clone());
            join_set.spawn(Self::commit_account(
                me,
                commitable,
                ephemeral_blockhash,
                commit_strategy,
                finalize,
            ));
        }
        join_set.join_all().await
    }

    async fn commit_account(
        me: Arc<Self>,
        mut commitable: CommitableAccount,
        ephemeral_blockhash: Hash,
        commit_strategy: CommitStrategy,
        finalize: bool,
    ) -> CommitAccountResult<CommitInfo> {
        let commit_info = if commitable.has_data() {
            let chunks =
                Chunks::new(commitable.chunk_count(), commitable.chunk_size());
            let chunks_account_size = to_vec(&chunks).unwrap().len() as u64;

            // Initialize the Changeset and Chunks accounts on chain
            let buffer_account_size = commitable.size() as u64;

            let (init_ix, chunks_pda, buffer_pda) =
                create_init_ix(CreateInitIxArgs {
                    authority: me.authority.pubkey(),
                    pubkey: commitable.pubkey,
                    chunks_account_size,
                    buffer_account_size,
                    blockhash: ephemeral_blockhash,
                    chunk_count: commitable.chunk_count(),
                    chunk_size: commitable.chunk_size(),
                });
            let realloc_ixs =
                create_realloc_buffer_ixs(CreateReallocBufferIxArgs {
                    authority: me.authority.pubkey(),
                    pubkey: commitable.pubkey,
                    buffer_account_size,
                    blockhash: ephemeral_blockhash,
                });

            let commit_info = CommitInfo::BufferedDataAccount {
                pubkey: commitable.pubkey,
                commit_state: commit_state_pda_from_delegated_account(
                    &commitable.pubkey,
                ),
                delegated_account_owner: commitable.delegated_account_owner,
                slot: commitable.slot,
                ephemeral_blockhash,
                undelegate: commitable.undelegate,
                chunks_pda,
                buffer_pda,
                lamports: commitable.lamports,
                bundle_id: commitable.bundle_id,
                finalize,
            };

            // Even though this transaction also inits the chunks account we check
            // that it succeeded by querying the buffer account since this is the
            // only of the two that we may have to realloc.
            let commit_info = Arc::new(
                me.init_accounts(
                    init_ix,
                    realloc_ixs,
                    commitable.pubkey,
                    &buffer_pda,
                    buffer_account_size,
                    ephemeral_blockhash,
                    commit_info,
                    commit_strategy,
                )
                .await?,
            );

            let mut last_write_chunks_err = None;
            if let Err(err) = me
                .write_chunks(
                    commitable.pubkey,
                    commitable.iter_all(),
                    ephemeral_blockhash,
                )
                .await
            {
                last_write_chunks_err = Some(err);
            };

            let mut remaining_tries = 10;
            const MAX_GET_ACCOUNT_RETRIES: usize = 5;
            loop {
                let mut acc = None;
                let mut last_get_account_err = None;
                for _ in 0..MAX_GET_ACCOUNT_RETRIES {
                    match me
                        .magicblock_rpc_client
                        .get_account(&chunks_pda)
                        .await
                    {
                        Ok(Some(x)) => {
                            acc.replace(x);
                            break;
                        }
                        Ok(None) => {
                            me.wait_for_account("chunks account", None).await
                        }
                        Err(err) => {
                            me.wait_for_account("chunks account", Some(&err))
                                .await;
                            last_get_account_err.replace(err);
                        }
                    }
                }
                let Some(acc) = acc else {
                    return Err(CommitAccountError::GetChunksAccount(
                        last_get_account_err,
                        commit_info.clone(),
                        commit_strategy,
                    ));
                };
                let chunks =
                    Chunks::try_from_slice(&acc.data).map_err(|err| {
                        CommitAccountError::DeserializeChunksAccount(
                            err,
                            commit_info.clone(),
                            commit_strategy,
                        )
                    })?;

                if chunks.is_complete() {
                    break;
                }

                remaining_tries -= 1;
                if remaining_tries == 0 {
                    return Err(
                        CommitAccountError::WriteChunksRanOutOfRetries(
                            last_write_chunks_err,
                            commit_info.clone(),
                            commit_strategy,
                        ),
                    );
                }
                commitable.set_chunks(chunks);
                if let Err(err) = me
                    .write_chunks(
                        commitable.pubkey,
                        commitable.iter_missing(),
                        ephemeral_blockhash,
                    )
                    .await
                {
                    last_write_chunks_err = Some(err);
                }
            }
            commit_info
        } else {
            Arc::new(CommitInfo::EmptyAccount {
                pubkey: commitable.pubkey,
                delegated_account_owner: commitable.delegated_account_owner,
                slot: commitable.slot,
                ephemeral_blockhash,
                undelegate: commitable.undelegate,
                lamports: commitable.lamports,
                bundle_id: commitable.bundle_id,
                finalize,
            })
        };

        let commit_info = Arc::<CommitInfo>::unwrap_or_clone(commit_info);

        Ok(commit_info)
    }

    /// Sends init/realloc transactions until the account has the desired size
    /// - `init_ix` - the instruction to initialize the buffer and chunk account
    /// - `realloc_ixs` - the instructions to realloc the buffer account until it reaches the
    ///   size needed to store the account's data
    /// - `pubkey` - the pubkey of the account whose data we are storing
    /// - `buffer_pda` - the address of the account where we buffer the data to be committed
    /// - `buffer_account_size` - the size of the buffer account
    /// - `ephemeral_blockhash` - the blockhash in the ephemeral at which we are committing
    /// - `commit_info` - the commit info to be returned or included in errors
    /// - `commit_strategy` - the commit strategy that is used
    #[allow(clippy::too_many_arguments)] // private method
    async fn init_accounts(
        &self,
        init_ix: Instruction,
        realloc_ixs: Vec<Instruction>,
        pubkey: Pubkey,
        buffer_pda: &Pubkey,
        buffer_account_size: u64,
        ephemeral_blockhash: Hash,
        commit_info: CommitInfo,
        commit_strategy: CommitStrategy,
    ) -> CommitAccountResult<CommitInfo> {
        // We cannot allocate more than MAX_INITIAL_BUFFER_SIZE in a single
        // instruction. Therefore we append a realloc instruction if the buffer
        // is very large.
        // init_ixs is the init ix with as many realloc ixs as fit into one tx
        // extra_realloc_ixs are the remaining realloc ixs that need to be sent
        // in separate transactions
        let (init_ix_chunk, extra_realloc_ix_chunks) = {
            let mut chunked_ixs = chunk_realloc_ixs(realloc_ixs, Some(init_ix));
            let init_with_initial_reallocs = chunked_ixs.remove(0);
            let remaining_reallocs = if chunked_ixs.is_empty() {
                None
            } else {
                Some(chunked_ixs)
            };
            (init_with_initial_reallocs, remaining_reallocs)
        };

        debug!(
            "Init+Realloc chunk ixs {}, Extra Realloc Chunks {}",
            init_ix_chunk.len(),
            extra_realloc_ix_chunks.as_ref().map_or(0, |x| x.len())
        );

        // First ensure that the tx including the init ix lands
        let mut init_sig = None;
        let mut last_err = None;
        const MAX_RETRIES: usize = 2;
        'land_init_transaction: for _ in 0..MAX_RETRIES {
            // Only retry the init transaction if it failed to send and confirm
            if init_sig.is_none() {
                let init_budget_ixs = self
                    .compute_budget_config
                    .buffer_init
                    .instructions(init_ix_chunk.len() - 1);
                match send_and_confirm(
                    self.magicblock_rpc_client.clone(),
                    self.authority.insecure_clone(),
                    [init_budget_ixs, init_ix_chunk.clone()].concat(),
                    "init buffer and chunk account".to_string(),
                    None,
                    MagicBlockSendTransactionConfig::ensure_committed(),
                    None,
                )
                .await
                {
                    Err(err) => {
                        last_err = Some(err);
                        continue;
                    }
                    Ok(sig) => {
                        init_sig = Some(sig);
                    }
                }
            }

            // At this point the transaction was confirmed and we should be able
            // to get the initialized pda and chunk account
            const MAX_GET_ACCOUNT_RETRIES: usize = 5;
            for _ in 0..MAX_GET_ACCOUNT_RETRIES {
                match self.magicblock_rpc_client.get_account(buffer_pda).await {
                    Ok(Some(_)) => {
                        // The account was initialized
                        break 'land_init_transaction;
                    }
                    Ok(None) => {
                        self.wait_for_account("buffer account", None).await
                    }
                    Err(err) => {
                        self.wait_for_account("buffer account", Some(&err))
                            .await
                    }
                }
            }
        } // 'land_init_transaction

        if init_sig.is_none() {
            let err = last_err
                .as_ref()
                .map(|x| x.to_string())
                .unwrap_or("Unknown Error".to_string());
            return Err(CommitAccountError::InitBufferAndChunkAccounts(
                err,
                Box::new(commit_info),
                commit_strategy,
            ));
        }

        // After that we can ensure all extra reallocs in parallel
        if let Some(realloc_ixs) = extra_realloc_ix_chunks {
            let mut next_reallocs = self
                .run_reallocs(
                    buffer_pda,
                    realloc_ixs,
                    buffer_account_size,
                    buffer_account_size,
                    0,
                )
                .await;

            if next_reallocs.is_some() {
                let args = CreateReallocBufferIxArgs {
                    authority: self.authority.pubkey(),
                    pubkey,
                    buffer_account_size,
                    blockhash: ephemeral_blockhash,
                };

                while let Some(NextReallocs {
                    missing_size,
                    start_idx,
                }) = next_reallocs
                {
                    let realloc_ixs = {
                        let realloc_ixs =
                            create_realloc_buffer_ixs_to_add_remaining(
                                &args,
                                missing_size,
                            );

                        chunk_realloc_ixs(realloc_ixs, None)
                    };
                    next_reallocs = self
                        .run_reallocs(
                            buffer_pda,
                            realloc_ixs,
                            buffer_account_size,
                            missing_size,
                            start_idx,
                        )
                        .await;
                    // TODO(thlorenz): give up at some point
                }
            }
        }

        Ok(commit_info)
    }

    /// Returns the size that still needs to be allocated after running the instructions
    /// along with the idx at which we start (in order to keep increasing the idx of realloc
    /// attempt).
    /// Returns `None` once the desired size is reached and we're done.
    async fn run_reallocs(
        &self,
        pda: &Pubkey,
        realloc_ixs: Vec<Vec<Instruction>>,
        desired_size: u64,
        missing_size: u64,
        start_idx: usize,
    ) -> Option<NextReallocs> {
        let mut join_set = JoinSet::new();
        let count = realloc_ixs.len();
        let latest_blockhash =
            match self.magicblock_rpc_client.get_latest_blockhash().await {
                Ok(hash) => hash,
                Err(err) => {
                    error!(
                        "Failed to get latest blockhash to run reallocs: {:?}",
                        err
                    );
                    return Some(NextReallocs {
                        missing_size,
                        start_idx,
                    });
                }
            };
        for (idx, ixs) in realloc_ixs.into_iter().enumerate() {
            let authority = self.authority.insecure_clone();
            let rpc_client = self.magicblock_rpc_client.clone();
            let realloc_budget_ixs = self
                .compute_budget_config
                .buffer_realloc
                .instructions(ixs.len());
            // NOTE: we ignore failures to send/confirm realloc transactions and just
            // keep calling [CommittorProcessor::run_reallocs] until we reach the desired size
            join_set.spawn(async move {
                send_and_confirm(
                    rpc_client,
                    authority,
                    [realloc_budget_ixs, ixs].concat(),
                    format!(
                        "realloc buffer account {}/{}",
                        start_idx + idx,
                        start_idx + count
                    ),
                    Some(latest_blockhash),
                    MagicBlockSendTransactionConfig::ensure_processed(),
                    None,
                )
                .await
                .inspect_err(|err| {
                    warn!("{:?}", err);
                })
            });
        }
        join_set.join_all().await;

        match self.magicblock_rpc_client.get_account(pda).await {
            Ok(Some(acc)) => {
                // Once the account has the desired size we are done
                let current_size = acc.data.len();
                if current_size as u64 >= desired_size {
                    None
                } else {
                    Some(desired_size - current_size as u64)
                }
            }
            // NOTE: if we cannot get the account we must assume that
            //       the entire size we just tried to alloc is still missing
            Ok(None) => {
                warn!("buffer account not found");
                Some(missing_size)
            }
            Err(err) => {
                warn!("Failed to get buffer account: {:?}", err);
                Some(missing_size)
            }
        }
        .map(|missing_size| NextReallocs {
            missing_size,
            start_idx: count,
        })
    }

    /// Sends a transaction to write each chunk.
    /// Initially it gets latest blockhash and errors if that fails.
    /// All other errors while sending the transaction are logged and ignored.
    /// The chunks whose write transactions failed are expected to be retried in
    /// the next run.
    /// - `pubkey` - the on chain pubkey of the account whose data we are writing to the buffer
    /// - `chunks` - the chunks to write
    /// - `ephemeral_blockhash` - the blockhash to use for the transaction
    async fn write_chunks<Iter: IntoIterator<Item = ChangesetChunk>>(
        &self,
        pubkey: Pubkey,
        chunks: Iter,
        ephemeral_blockhash: Hash,
    ) -> MagicBlockRpcClientResult<()> {
        let mut join_set = JoinSet::new();

        let latest_blockhash =
            self.magicblock_rpc_client.get_latest_blockhash().await?;

        for chunk in chunks.into_iter() {
            let authority = self.authority.insecure_clone();
            let rpc_client = self.magicblock_rpc_client.clone();
            let chunk_bytes = chunk.data_chunk.len();
            let ix = create_write_ix(CreateWriteIxArgs {
                authority: authority.pubkey(),
                pubkey,
                offset: chunk.offset,
                data_chunk: chunk.data_chunk,
                blockhash: ephemeral_blockhash,
            });
            let write_budget_ixs = self
                .compute_budget_config
                .buffer_write
                .instructions(chunk_bytes);
            // NOTE: we ignore failures to send/confirm write transactions and just
            // keep calling [CommittorProcessor::write_chunks] until all of them are
            // written which is verified via the chunks account
            join_set.spawn(async move {
                send_and_confirm(
                    rpc_client,
                    authority,
                    [write_budget_ixs, vec![ix]].concat(),
                    format!("write chunk for offset {}", chunk.offset),
                    Some(latest_blockhash),
                    // NOTE: We could use `processed` here and wait to get the processed status at
                    // least which would make things a bit slower.
                    // However that way we would avoid sending unnecessary transactions potentially
                    // since we may not see some written chunks yet when we get the chunks account.
                    MagicBlockSendTransactionConfig::ensure_processed(),
                    None,
                )
                .await
                .inspect_err(|err| {
                    error!("{:?}", err);
                })
            });
        }
        if log::log_enabled!(log::Level::Trace) {
            trace!("Writing {} chunks", join_set.len());
        }

        join_set.join_all().await;

        Ok(())
    }

    async fn wait_for_account(
        &self,
        account_label: &str,
        err: Option<&MagicBlockRpcClientError>,
    ) {
        let sleep_time_ms = {
            if let Some(err) = err {
                error!("Failed to {} account: {:?}", account_label, err);
            } else {
                warn!("Failed to {} account", account_label);
            }
            100
        };
        tokio::time::sleep(Duration::from_millis(sleep_time_ms)).await;
    }
}
