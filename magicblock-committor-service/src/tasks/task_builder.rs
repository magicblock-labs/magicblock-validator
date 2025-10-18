use std::sync::Arc;

use async_trait::async_trait;
use dlp::args::Context;
use light_client::indexer::{
    photon_indexer::PhotonIndexer, Indexer, IndexerError,
};
use light_sdk::{
    error::LightSdkError,
    instruction::{
        account_meta::CompressedAccountMeta, PackedAccounts,
        SystemAccountMetaConfig, ValidityProof,
    },
};
use log::*;
use magicblock_core::compression::derive_cda_from_pda;
use magicblock_program::magic_scheduled_base_intent::{
    CommitType, CommittedAccount, MagicBaseIntent, ScheduledBaseIntent,
    UndelegateType,
};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::AccountMeta;

use crate::{
    intent_executor::task_info_fetcher::{
        TaskInfoFetcher, TaskInfoFetcherError,
    },
    persist::IntentPersister,
    tasks::{
        args_task::{ArgsTask, ArgsTaskType},
        BaseActionTask, BaseTask, CommitTask, CompressedCommitTask,
        CompressedFinalizeTask, CompressedUndelegateTask, FinalizeTask,
        UndelegateTask,
    },
};

#[derive(Clone, Debug)]
pub struct CompressedData {
    pub hash: [u8; 32],
    pub compressed_delegation_record_bytes: Vec<u8>,
    pub remaining_accounts: Vec<AccountMeta>,
    pub account_meta: CompressedAccountMeta,
    pub proof: ValidityProof,
}

#[async_trait]
pub trait TasksBuilder {
    // Creates tasks for commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        persister: &Option<P>,
        photon_client: &Option<Arc<PhotonIndexer>>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>>;

    // Create tasks for finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        photon_client: &Option<Arc<PhotonIndexer>>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>>;
}

/// V1 Task builder
/// V1: Actions are part of finalize tx
pub struct TaskBuilderImpl;

#[async_trait]
impl TasksBuilder for TaskBuilderImpl {
    /// Returns [`Task`]s for Commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        persister: &Option<P>,
        photon_client: &Option<Arc<PhotonIndexer>>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        let (accounts, allow_undelegation, compressed) = match &base_intent
            .base_intent
        {
            MagicBaseIntent::BaseActions(actions) => {
                let tasks = actions
                    .iter()
                    .map(|el| {
                        let task = BaseActionTask {
                            context: Context::Standalone,
                            action: el.clone(),
                        };
                        Box::new(ArgsTask::new(ArgsTaskType::BaseAction(task)))
                            as Box<dyn BaseTask>
                    })
                    .collect();

                return Ok(tasks);
            }
            MagicBaseIntent::Commit(t) => {
                (t.get_committed_accounts(), false, false)
            }
            MagicBaseIntent::CommitAndUndelegate(t) => {
                (t.commit_action.get_committed_accounts(), true, false)
            }
            MagicBaseIntent::CompressedCommit(t) => {
                (t.get_committed_accounts(), false, true)
            }
            MagicBaseIntent::CompressedCommitAndUndelegate(t) => {
                (t.commit_action.get_committed_accounts(), true, true)
            }
        };

        let committed_pubkeys = accounts
            .iter()
            .map(|account| account.pubkey)
            .collect::<Vec<_>>();
        let commit_ids = commit_id_fetcher
            .fetch_next_commit_ids(&committed_pubkeys, compressed)
            .await
            .map_err(TaskBuilderError::CommitTasksBuildError)?;

        // Persist commit ids for commitees
        commit_ids
            .iter()
            .for_each(|(pubkey, commit_id)| {
                if let Err(err) = persister.set_commit_id(base_intent.id, pubkey, *commit_id) {
                    error!("Failed to persist commit id: {}, for message id: {} with pubkey {}: {}", commit_id, base_intent.id, pubkey, err);
                }
            });

        let tasks = if compressed {
            // For compressed accounts, prepare compression data
            let photon_client = photon_client
                .as_ref()
                .ok_or(TaskBuilderError::PhotonClientNotFound)?;
            let mut compressed_results = vec![];
            for account in accounts {
                compressed_results.push(
                    get_compressed_data(&account.pubkey, &photon_client).await,
                );
            }

            accounts.iter().zip(compressed_results).map(|(account, compressed_data)| {
                let commit_id = *commit_ids.get(&account.pubkey).expect("CommitIdFetcher provide commit ids for all listed pubkeys, or errors!");
                let compressed_data = compressed_data.expect("Compressed commit task must be provided with compressed data");
                let task = ArgsTaskType::CompressedCommit(CompressedCommitTask {
                    commit_id,
                    allow_undelegation,
                    committed_account: account.clone(),
                    compressed_data
                });
                Box::new(ArgsTask::new(task)) as Box<dyn BaseTask>
            })
            .collect()
        } else {
            accounts
            .iter()
            .map(|account| {
                let commit_id = *commit_ids.get(&account.pubkey).expect("CommitIdFetcher provide commit ids for all listed pubkeys, or errors!");
                let task = ArgsTaskType::Commit(CommitTask {
                    commit_id,
                    allow_undelegation,
                    committed_account: account.clone(),
                });

                Box::new(ArgsTask::new(task)) as Box<dyn BaseTask>
            })
            .collect()
        };

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        photon_client: &Option<Arc<PhotonIndexer>>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        let is_compressed = base_intent.is_compressed();
        // Helper to create a finalize task
        let finalize_task = |account: &CommittedAccount,
                             compressed_data: Option<CompressedData>|
         -> Box<dyn BaseTask> {
            if is_compressed {
                let compressed_data =
                    compressed_data.expect("Compressed finalize task must be provided with compressed data");
                let task_type =
                    ArgsTaskType::CompressedFinalize(CompressedFinalizeTask {
                        delegated_account: account.pubkey,
                        compressed_data,
                    });
                Box::new(ArgsTask::new(task_type))
            } else {
                let task_type = ArgsTaskType::Finalize(FinalizeTask {
                    delegated_account: account.pubkey,
                });
                Box::new(ArgsTask::new(task_type))
            }
        };

        // Helper to create an undelegate task
        let undelegate_task = |account: &CommittedAccount,
                               rent_reimbursement: Option<&Pubkey>,
                               compressed_data: Option<CompressedData>|
         -> Box<dyn BaseTask> {
            if is_compressed {
                let compressed_data = compressed_data.expect("Compressed undelegate task must be provided with compressed data");
                let task_type = ArgsTaskType::CompressedUndelegate(
                    CompressedUndelegateTask {
                        delegated_account: account.pubkey,
                        owner_program: account.account.owner,
                        compressed_data,
                    },
                );
                Box::new(ArgsTask::new(task_type))
            } else {
                let task_type = ArgsTaskType::Undelegate(UndelegateTask {
                    delegated_account: account.pubkey,
                    owner_program: account.account.owner,
                    rent_reimbursement: *rent_reimbursement.unwrap(),
                });
                Box::new(ArgsTask::new(task_type))
            }
        };

        // Helper to process commit types
        let process_commit = async |commit: &CommitType,
                                    photon_client: &Option<
            Arc<PhotonIndexer>,
        >| {
            match commit {
                CommitType::Standalone(committed_accounts) if is_compressed => {
                    let mut compressed_data = vec![];
                    let photon_client = photon_client
                        .as_ref()
                        .ok_or(TaskBuilderError::PhotonClientNotFound)?;
                    for account in committed_accounts {
                        compressed_data.push(
                            get_compressed_data(
                                &account.pubkey,
                                &photon_client,
                            )
                            .await
                            .ok(),
                        );
                    }

                    Ok(committed_accounts
                        .iter()
                        .zip(compressed_data)
                        .map(|(account, compressed_data)| {
                            finalize_task(account, compressed_data)
                        })
                        .collect())
                }
                CommitType::Standalone(accounts) => Ok(accounts
                    .iter()
                    .map(|account| finalize_task(account, None))
                    .collect()),
                CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions,
                    ..
                } => {
                    let mut compressed_data = vec![];
                    for account in committed_accounts {
                        if is_compressed {
                            let photon_client = photon_client.as_ref().ok_or(
                                TaskBuilderError::PhotonClientNotFound,
                            )?;
                            compressed_data.push(
                                get_compressed_data(
                                    &account.pubkey,
                                    photon_client,
                                )
                                .await
                                .ok(),
                            );
                        } else {
                            compressed_data.push(None);
                        }
                    }

                    let mut tasks = committed_accounts
                        .iter()
                        .zip(compressed_data)
                        .map(|(account, compressed_data)| {
                            finalize_task(account, compressed_data)
                        })
                        .collect::<Vec<_>>();
                    tasks.extend(base_actions.iter().map(|action| {
                        let task = BaseActionTask {
                            context: Context::Commit,
                            action: action.clone(),
                        };
                        let task_type = ArgsTaskType::BaseAction(task);
                        Box::new(ArgsTask::new(task_type)) as Box<dyn BaseTask>
                    }));
                    Ok(tasks)
                }
            }
        };

        match &base_intent.base_intent {
            MagicBaseIntent::BaseActions(_) => Ok(vec![]),
            MagicBaseIntent::Commit(commit)
            | MagicBaseIntent::CompressedCommit(commit) => {
                Ok(process_commit(commit, photon_client).await?)
            }
            MagicBaseIntent::CommitAndUndelegate(t) => {
                let mut tasks =
                    process_commit(&t.commit_action, photon_client).await?;

                // Get rent reimbursments for undelegated accounts
                let accounts = t.get_committed_accounts();
                let rent_reimbursements = info_fetcher
                    .fetch_rent_reimbursements(
                        &accounts
                            .iter()
                            .map(|account| account.pubkey)
                            .collect::<Vec<_>>(),
                    )
                    .await
                    .map_err(TaskBuilderError::FinalizedTasksBuildError)?;

                tasks.extend(accounts.iter().zip(rent_reimbursements).map(
                    |(account, rent_reimbursement)| {
                        undelegate_task(
                            account,
                            Some(&rent_reimbursement),
                            None,
                        )
                    },
                ));

                match &t.undelegate_action {
                    UndelegateType::Standalone => Ok(tasks),
                    UndelegateType::WithBaseActions(actions) => {
                        tasks.extend(actions.iter().map(|action| {
                            let task = BaseActionTask {
                                context: Context::Undelegate,
                                action: action.clone(),
                            };
                            let task =
                                ArgsTask::new(ArgsTaskType::BaseAction(task));
                            Box::new(task) as Box<dyn BaseTask>
                        }));

                        Ok(tasks)
                    }
                }
            }
            MagicBaseIntent::CompressedCommitAndUndelegate(t) => {
                let mut tasks =
                    process_commit(&t.commit_action, photon_client).await?;

                // TODO: Compressed undelegate is not supported yet
                // This is because the validator would have to pay rent out of pocket.
                // This could be solved by using the ephemeral payer to ensure the user can pay the rent.

                // tasks.extend(
                //     t.get_committed_accounts()
                //         .iter()
                //         .map(|account| undelegate_task(account, None, None)),
                // );

                match &t.undelegate_action {
                    UndelegateType::Standalone => Ok(tasks),
                    UndelegateType::WithBaseActions(actions) => {
                        tasks.extend(actions.iter().map(|action| {
                            let task = BaseActionTask {
                                context: Context::Undelegate,
                                action: action.clone(),
                            };
                            let task =
                                ArgsTask::new(ArgsTaskType::BaseAction(task));
                            Box::new(task) as Box<dyn BaseTask>
                        }));

                        Ok(tasks)
                    }
                }
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TaskBuilderError {
    #[error("CommitIdFetchError: {0}")]
    CommitTasksBuildError(#[source] TaskInfoFetcherError),
    #[error("FinalizedTasksBuildError: {0}")]
    FinalizedTasksBuildError(#[source] TaskInfoFetcherError),
    #[error("CompressedDataFetchError: {0}")]
    CompressedDataFetchError(#[source] IndexerError),
    #[error("LightSdkError: {0}")]
    LightSdkError(#[source] LightSdkError),
    #[error("MissingStateTrees")]
    MissingStateTrees,
    #[error("MissingAddress")]
    MissingAddress,
    #[error("MissingCompressedData")]
    MissingCompressedData,
    #[error("Photon client not found")]
    PhotonClientNotFound,
}

pub type TaskBuilderResult<T, E = TaskBuilderError> = Result<T, E>;

pub(crate) async fn get_compressed_data(
    pubkey: &Pubkey,
    photon_client: &PhotonIndexer,
) -> Result<CompressedData, TaskBuilderError> {
    debug!("Getting compressed data for pubkey: {}", pubkey);
    let cda = derive_cda_from_pda(pubkey);
    let compressed_delegation_record = photon_client
        .get_compressed_account(cda.to_bytes(), None)
        .await
        .map_err(TaskBuilderError::CompressedDataFetchError)?
        .value;
    let proof_result = photon_client
        .get_validity_proof(
            vec![compressed_delegation_record.hash],
            vec![],
            None,
        )
        .await
        .map_err(TaskBuilderError::CompressedDataFetchError)?
        .value;

    let system_account_meta_config =
        SystemAccountMetaConfig::new(compressed_delegation_client::ID);
    let mut remaining_accounts = PackedAccounts::default();
    remaining_accounts
        .add_system_accounts_v2(system_account_meta_config)
        .map_err(TaskBuilderError::LightSdkError)?;
    let packed_tree_accounts = proof_result
        .pack_tree_infos(&mut remaining_accounts)
        .state_trees
        .ok_or(TaskBuilderError::MissingStateTrees)?;

    let account_meta = CompressedAccountMeta {
        tree_info: packed_tree_accounts.packed_tree_infos[0],
        address: compressed_delegation_record
            .address
            .ok_or(TaskBuilderError::MissingAddress)?,
        output_state_tree_index: packed_tree_accounts.output_tree_index,
    };
    debug!("Compressed data obtained!");
    debug!("Compressed data: {:?}", account_meta);
    debug!(
        "Remaining accounts: {:?}",
        remaining_accounts.to_account_metas().0
    );
    debug!("Proof: {:?}", proof_result.proof);
    debug!(
        "Compressed delegation record: {:?}",
        compressed_delegation_record
    );
    Ok(CompressedData {
        hash: compressed_delegation_record.hash,
        compressed_delegation_record_bytes: compressed_delegation_record
            .data
            .ok_or(TaskBuilderError::MissingCompressedData)?
            .data
            .clone(),
        remaining_accounts: remaining_accounts.to_account_metas().0.clone(),
        account_meta: account_meta,
        proof: proof_result.proof,
    })
}
