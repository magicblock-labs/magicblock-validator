use std::{collections::HashSet, sync::Arc};

use dlp::args::CommitStateArgs;
use log::*;
use magicblock_committor_program::Changeset;
use magicblock_rpc_client::MagicBlockSendTransactionConfig;
use solana_sdk::{hash::Hash, signer::Signer};

use super::CommittorProcessor;
use crate::{
    commit::common::{
        get_accounts_to_undelegate, lookup_table_keys, send_and_confirm,
    },
    commit_stage::{CommitSignatures, CommitStage},
    persist::CommitStrategy,
    undelegate::undelegate_commitables_ixs,
    CommitInfo,
};

impl CommittorProcessor {
    /// Commits a changeset directly using args to include the commit state
    /// - **changeset**: the changeset to commit
    /// - **finalize**: whether to finalize the commit
    /// - **finalize_separately**: whether to finalize the commit in a separate transaction, if
    ///   this is `false` we can include the finalize instructions with the process instructions
    /// - **ephemeral_blockhash**: the ephemeral blockhash to use for the commit
    /// - **latest_blockhash**: the latest blockhash on chain to use for the commit
    /// - **use_lookup**: whether to use the lookup table for the instructions
    pub async fn commit_changeset_using_args(
        me: Arc<CommittorProcessor>,
        changeset: Changeset,
        (finalize, finalize_separately): (bool, bool),
        ephemeral_blockhash: Hash,
        latest_blockhash: Hash,
        use_lookup: bool,
    ) -> Vec<CommitStage> {
        // Each changeset is expected to fit into a single instruction which was ensured
        // when splitting the original changeset

        let mut process_ixs = Vec::new();
        let mut finalize_ixs = Vec::new();
        let owners = changeset.owners();
        let accounts_to_undelegate =
            get_accounts_to_undelegate(&changeset, finalize);
        let commitables = changeset.into_committables(0);
        // NOTE: we copy the commitables here in order to return them with an error
        //      [CommitStage] if needed. Since the data of these accounts is small
        //      (< 1024 bytes), it is acceptable perf overhead
        //      Alternatively we could include only metadata for the [CommitStage].
        for commitable in commitables.iter() {
            let commit_args = CommitStateArgs {
                slot: commitable.slot,
                lamports: commitable.lamports,
                allow_undelegation: commitable.undelegate,
                data: commitable.data.clone(),
            };

            let ix = dlp::instruction_builder::commit_state(
                me.authority.pubkey(),
                commitable.pubkey,
                commitable.delegated_account_owner,
                commit_args,
            );
            process_ixs.push(ix);

            // We either include the finalize instructions with the process instruction or
            // if the strategy builder determined that they wouldn't fit then we run them
            // in a separate transaction
            if finalize {
                let finalize_ix = dlp::instruction_builder::finalize(
                    me.authority.pubkey(),
                    commitable.pubkey,
                );
                if finalize_separately {
                    finalize_ixs.push(finalize_ix);
                } else {
                    process_ixs.push(finalize_ix);
                }
            }
        }

        let commit_infos = commitables
            .into_iter()
            .map(|acc| {
                CommitInfo::from_small_data_account(
                    acc,
                    ephemeral_blockhash,
                    finalize,
                )
            })
            .collect::<Vec<_>>();

        let committees = commit_infos
            .iter()
            .map(|x| x.pubkey())
            .collect::<HashSet<_>>();

        let table_mania = use_lookup.then(|| me.table_mania.clone());
        let table_mania_setup = table_mania.as_ref().map(|tm| {
            let keys = lookup_table_keys(&me.authority, &committees, &owners);
            (tm, keys)
        });

        let compute_budget_ixs = me
            .compute_budget_config
            .args_process_budget()
            .instructions(committees.len());
        let process_sig = match send_and_confirm(
            me.magicblock_rpc_client.clone(),
            me.authority.insecure_clone(),
            [compute_budget_ixs, process_ixs].concat(),
            "commit changeset using args".to_string(),
            Some(latest_blockhash),
            MagicBlockSendTransactionConfig::ensure_committed(),
            table_mania_setup.clone(),
        )
        .await
        {
            Ok(sig) => sig,
            Err(err) => {
                error!("Failed to commit changeset with {} accounts using args: {:?}", committees.len(), err);
                let strategy = CommitStrategy::args(use_lookup);
                let sigs = err.signature().map(|sig| CommitSignatures {
                    process_signature: sig,
                    finalize_signature: None,
                    undelegate_signature: None,
                });
                return commit_infos
                    .into_iter()
                    .map(|x| {
                        CommitStage::FailedProcess((
                            x,
                            strategy,
                            sigs.as_ref().cloned(),
                        ))
                    })
                    .collect();
            }
        };

        let finalize_sig = if !finalize_ixs.is_empty() {
            let table_mania_setup = table_mania.as_ref().map(|tm| {
                let keys =
                    lookup_table_keys(&me.authority, &committees, &owners);
                (tm, keys)
            });
            let finalize_budget_ixs = me
                .compute_budget_config
                .finalize_budget()
                .instructions(committees.len());
            match send_and_confirm(
                me.magicblock_rpc_client.clone(),
                me.authority.insecure_clone(),
                [finalize_budget_ixs, finalize_ixs].concat(),
                "commit changeset using args".to_string(),
                Some(latest_blockhash),
                MagicBlockSendTransactionConfig::ensure_committed(),
                table_mania_setup,
            )
            .await
            {
                Ok(sig) => Some(sig),
                Err(err) => {
                    error!(
                        "Failed to finalize changeset using args: {:?}",
                        err
                    );
                    return commit_infos
                        .into_iter()
                        .map(|x| {
                            CommitStage::FailedFinalize((
                                x,
                                CommitStrategy::args(use_lookup),
                                CommitSignatures {
                                    process_signature: process_sig,
                                    finalize_signature: err.signature(),
                                    undelegate_signature: None,
                                },
                            ))
                        })
                        .collect();
                }
            }
        } else {
            (!finalize_separately).then_some(process_sig)
        };

        trace!(
            "Successfully processed {} commit infos via transaction '{}'",
            commit_infos.len(),
            process_sig
        );

        let undelegate_sig = if let Some(sig) = finalize_sig {
            trace!(
                "Successfully finalized {} commit infos via transaction '{}'",
                commit_infos.len(),
                sig
            );

            // If we successfully finalized the commit then we can undelegate accounts
            if let Some(accounts) = accounts_to_undelegate {
                let accounts_len = accounts.len();
                let undelegate_ixs = match undelegate_commitables_ixs(
                    &me.magicblock_rpc_client,
                    me.authority.pubkey(),
                    accounts,
                )
                .await
                {
                    Ok(ixs) => ixs.into_values().collect::<Vec<_>>(),
                    Err(err) => {
                        error!(
                        "Failed to prepare accounts undelegation '{}': {:?}",
                        err, err
                    );
                        return commit_infos
                            .into_iter()
                            .map(|x| {
                                CommitStage::FailedUndelegate((
                                    x,
                                    CommitStrategy::args(use_lookup),
                                    CommitSignatures {
                                        process_signature: process_sig,
                                        finalize_signature: finalize_sig,
                                        undelegate_signature: err.signature(),
                                    },
                                ))
                            })
                            .collect();
                    }
                };
                let undelegate_budget_ixs = me
                    .compute_budget_config
                    .undelegate_budget()
                    .instructions(accounts_len);
                match send_and_confirm(
                    me.magicblock_rpc_client.clone(),
                    me.authority.insecure_clone(),
                    [undelegate_budget_ixs, undelegate_ixs].concat(),
                    "undelegate committed accounts using args".to_string(),
                    Some(latest_blockhash),
                    MagicBlockSendTransactionConfig::ensure_committed(),
                    table_mania_setup,
                )
                .await
                {
                    Ok(sig) => {
                        trace!("Successfully undelegated accounts via transaction '{}'", sig);
                        Some(sig)
                    }
                    Err(err) => {
                        error!(
                        "Failed to undelegate accounts via transaction '{}': {:?}",
                        err, err
                    );
                        return commit_infos
                            .into_iter()
                            .map(|x| {
                                CommitStage::FailedUndelegate((
                                    x,
                                    CommitStrategy::args(use_lookup),
                                    CommitSignatures {
                                        process_signature: process_sig,
                                        finalize_signature: finalize_sig,
                                        undelegate_signature: err.signature(),
                                    },
                                ))
                            })
                            .collect();
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        commit_infos
            .into_iter()
            .map(|x| {
                CommitStage::Succeeded((
                    x,
                    CommitStrategy::args(use_lookup),
                    CommitSignatures {
                        process_signature: process_sig,
                        finalize_signature: finalize_sig,
                        undelegate_signature: undelegate_sig,
                    },
                ))
            })
            .collect()
    }
}
