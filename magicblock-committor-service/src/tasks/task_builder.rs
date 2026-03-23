use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{stream::FuturesUnordered, TryStreamExt};
use light_sdk::instruction::{
    account_meta::CompressedAccountMeta, ValidityProof,
};
use magicblock_program::magic_scheduled_base_intent::{
    CommitType, CommittedAccount, MagicBaseIntent, ScheduledBaseIntent,
    UndelegateType,
};
use solana_account::Account;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::error;

use super::{CommitDiffTask, CommitTask};
use crate::{
    intent_executor::task_info_fetcher::{
        TaskInfoFetcher, TaskInfoFetcherError,
    },
    persist::IntentPersister,
    tasks::{
        args_task::{ArgsTask, ArgsTaskType},
        task_strategist::TaskStrategistError,
        BaseActionTask, BaseTask, CompressedCommitTask, CompressedFinalizeTask,
        CompressedUndelegateTask, FinalizeTask, UndelegateTask,
    },
};

#[derive(Clone, Debug, Default, PartialEq)]
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
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>>;

    // Create tasks for finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>>;
}

/// V1 Task builder
/// V1: Actions are part of finalize tx
pub struct TaskBuilderImpl;

// Accounts larger than COMMIT_STATE_SIZE_THRESHOLD use CommitDiff to
// reduce instruction size. Below this threshold, the commit is sent
// as CommitState. The value (256) is chosen because it is sufficient
// for small accounts, which typically could hold up to 8 u32 fields or
// 4 u64 fields. These integers are expected to be on the hot path
// and updated continuously.
pub const COMMIT_STATE_SIZE_THRESHOLD: usize = 256;

impl TaskBuilderImpl {
    pub fn create_commit_task(
        commit_id: u64,
        allow_undelegation: bool,
        account: CommittedAccount,
        base_account: Option<Account>,
    ) -> ArgsTask {
        let base_account =
            if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
                base_account
            } else {
                None
            };

        if let Some(base_account) = base_account {
            ArgsTaskType::CommitDiff(CommitDiffTask {
                commit_id,
                allow_undelegation,
                committed_account: account,
                base_account,
            })
        } else {
            ArgsTaskType::Commit(CommitTask {
                commit_id,
                allow_undelegation,
                committed_account: account,
            })
        }
        .into()
    }
}

#[async_trait]
impl TasksBuilder for TaskBuilderImpl {
    /// Returns [`Task`]s for Commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        let (accounts, allow_undelegation, compressed) =
            match &base_intent.base_intent {
                MagicBaseIntent::BaseActions(actions) => {
                    let tasks = actions
                        .iter()
                        .map(|el| {
                            let task = BaseActionTask { action: el.clone() };
                            let task =
                                ArgsTask::new(ArgsTaskType::BaseAction(task));
                            Box::new(task) as Box<dyn BaseTask>
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

        let (commit_ids, base_accounts) = {
            let mut min_context_slot = 0;
            let committed_pubkeys = accounts
                .iter()
                .map(|account| {
                    min_context_slot =
                        std::cmp::max(min_context_slot, account.remote_slot);
                    account.pubkey
                })
                .collect::<Vec<_>>();

            let diffable_pubkeys = accounts
                .iter()
                .filter(|account| {
                    account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD
                })
                .map(|account| account.pubkey)
                .collect::<Vec<_>>();

            tokio::join!(
                info_fetcher.fetch_next_commit_ids(
                    &committed_pubkeys,
                    min_context_slot,
                    compressed
                ),
                info_fetcher.get_base_accounts(
                    diffable_pubkeys.as_slice(),
                    min_context_slot
                )
            )
        };

        let commit_ids =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;

        let base_accounts = match base_accounts {
            Ok(map) => map,
            Err(err) => {
                tracing::warn!(intent_id = base_intent.id, error = ?err, "Failed to fetch base accounts, falling back to CommitState");
                Default::default()
            }
        };

        // Persist commit ids for commitees
        commit_ids
            .iter()
            .for_each(|(pubkey, commit_id)| {
                if let Err(err) = persister.set_commit_id(base_intent.id, pubkey, *commit_id) {
                    error!(intent_id = base_intent.id, pubkey = %pubkey, error = ?err, "Failed to persist commit id");
                }
            });

        let tasks = if compressed {
            // For compressed accounts, prepare compression data
            accounts
                .iter()
                .map(|account| {
                    let commit_id = *commit_ids.get(&account.pubkey).ok_or(
                        TaskBuilderError::MissingCommitId(account.pubkey),
                    )?;
                    let account_clone = account.clone();
                    Ok(async move {
                        let compressed_data = info_fetcher
                            .get_compressed_data(&account_clone.pubkey, None)
                            .await?;
                        let task = ArgsTaskType::CompressedCommit(
                            CompressedCommitTask {
                                commit_id,
                                allow_undelegation,
                                committed_account: account_clone,
                                compressed_data,
                            },
                        );
                        Ok::<_, TaskBuilderError>(
                            Box::new(ArgsTask::new(task)) as Box<dyn BaseTask>
                        )
                    })
                })
                .collect::<Result<FuturesUnordered<_>, TaskBuilderError>>()?
                .try_collect()
                .await?
        } else {
            accounts
                .iter()
                .map(|account| {
                    let commit_id = *commit_ids.get(&account.pubkey).ok_or(
                        TaskBuilderError::MissingCommitId(account.pubkey),
                    )?;
                    // TODO (snawaz): if accounts do not have duplicate, then we can use remove
                    // instead:
                    //  let base_account = base_accounts.remove(&account.pubkey);
                    let base_account =
                        base_accounts.get(&account.pubkey).cloned();
                    let task = Self::create_commit_task(
                        commit_id,
                        allow_undelegation,
                        account.clone(),
                        base_account,
                    );
                    Ok::<_, TaskBuilderError>(
                        Box::new(task) as Box<dyn BaseTask>
                    )
                })
                .collect::<Result<_, _>>()?
        };

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        // Helper to create a finalize task
        fn finalize_task(
            account: &CommittedAccount,
            compressed_data: Option<CompressedData>,
        ) -> Box<dyn BaseTask> {
            if let Some(compressed_data) = compressed_data {
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
        }

        // Helper to create an undelegate task
        fn undelegate_task(
            account: &CommittedAccount,
            rent_reimbursement: &Pubkey,
            compressed_data: Option<CompressedData>,
        ) -> Box<dyn BaseTask> {
            if let Some(compressed_data) = compressed_data {
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
                    rent_reimbursement: *rent_reimbursement,
                });
                Box::new(ArgsTask::new(task_type))
            }
        }

        // Helper to get compressed data
        async fn get_compressed_data_for_accounts<C: TaskInfoFetcher>(
            info_fetcher: &Arc<C>,
            is_compressed: bool,
            committed_accounts: &[CommittedAccount],
        ) -> TaskBuilderResult<Vec<Option<CompressedData>>> {
            if is_compressed {
                committed_accounts
                    .iter()
                    .map(|account| {
                        let pubkey = account.pubkey;
                        async move {
                            Ok(Some(
                                info_fetcher
                                    .get_compressed_data(&pubkey, None)
                                    .await?,
                            ))
                        }
                    })
                    .collect::<FuturesUnordered<_>>()
                    .try_collect()
                    .await
            } else {
                Ok(vec![None; committed_accounts.len()])
            }
        }

        // Helper to process commit types
        async fn process_commit<C: TaskInfoFetcher>(
            info_fetcher: &Arc<C>,
            commit: &CommitType,
            is_compressed: bool,
        ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
            match commit {
                CommitType::Standalone(committed_accounts) => {
                    Ok(committed_accounts
                        .iter()
                        .zip(
                            get_compressed_data_for_accounts(
                                info_fetcher,
                                is_compressed,
                                committed_accounts,
                            )
                            .await?,
                        )
                        .map(|(account, compressed_data)| {
                            finalize_task(account, compressed_data)
                        })
                        .collect())
                }
                CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions,
                    ..
                } => {
                    let mut tasks = committed_accounts
                        .iter()
                        .zip(
                            get_compressed_data_for_accounts(
                                info_fetcher,
                                is_compressed,
                                committed_accounts,
                            )
                            .await?,
                        )
                        .map(|(account, compressed_data)| {
                            finalize_task(account, compressed_data)
                        })
                        .collect::<Vec<_>>();
                    tasks.extend(base_actions.iter().map(|action| {
                        let task = BaseActionTask {
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

        let is_compressed = base_intent.is_compressed();
        match &base_intent.base_intent {
            MagicBaseIntent::BaseActions(_) => Ok(vec![]),
            MagicBaseIntent::Commit(commit)
            | MagicBaseIntent::CompressedCommit(commit) => {
                Ok(process_commit(info_fetcher, commit, is_compressed).await?)
            }
            MagicBaseIntent::CommitAndUndelegate(t) => {
                let mut tasks = process_commit(
                    info_fetcher,
                    &t.commit_action,
                    is_compressed,
                )
                .await?;

                // Get rent reimbursments for undelegated accounts
                let accounts = t.get_committed_accounts();
                let mut min_context_slot = 0;
                let pubkeys = accounts
                    .iter()
                    .map(|account| {
                        min_context_slot = std::cmp::max(
                            min_context_slot,
                            account.remote_slot,
                        );
                        account.pubkey
                    })
                    .collect::<Vec<_>>();
                let rent_reimbursements = info_fetcher
                    .fetch_rent_reimbursements(&pubkeys, min_context_slot)
                    .await
                    .map_err(TaskBuilderError::FinalizedTasksBuildError)?;

                tasks.extend(accounts.iter().zip(rent_reimbursements).map(
                    |(account, rent_reimbursement)| {
                        undelegate_task(account, &rent_reimbursement, None)
                    },
                ));

                match &t.undelegate_action {
                    UndelegateType::Standalone => Ok(tasks),
                    UndelegateType::WithBaseActions(actions) => {
                        tasks.extend(actions.iter().map(|action| {
                            let task = BaseActionTask {
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
                let mut tasks = process_commit(
                    info_fetcher,
                    &t.commit_action,
                    is_compressed,
                )
                .await?;

                // TODO: Compressed undelegate is not supported yet
                // This is because the validator would have to pay rent out of pocket.
                // This could be solved by using the ephemeral payer to ensure the user can pay the rent.
                // https://github.com/magicblock-labs/magicblock-validator/issues/651

                // tasks.extend(accounts.iter().zip(rent_reimbursements).map(
                //     |(account, rent_reimbursement)| {
                //         undelegate_task(account, &rent_reimbursement, None)
                //     },
                // ));

                match &t.undelegate_action {
                    UndelegateType::Standalone => Ok(tasks),
                    UndelegateType::WithBaseActions(actions) => {
                        tasks.extend(actions.iter().map(|action| {
                            let task = BaseActionTask {
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
    #[error("TaskStrategistError: {0}")]
    TaskStrategistError(#[from] TaskStrategistError),
    #[error("MissingCommitId: {0}")]
    MissingCommitId(Pubkey),
    #[error("TaskInfoFetcherError: {0}")]
    TaskInfoFetcherError(#[from] TaskInfoFetcherError),
}

impl TaskBuilderError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::CommitTasksBuildError(err) => err.signature(),
            Self::FinalizedTasksBuildError(err) => err.signature(),
            Self::TaskStrategistError(_) => None,
            Self::MissingCommitId(_) => None,
            Self::TaskInfoFetcherError(err) => err.signature(),
        }
    }
}

pub type TaskBuilderResult<T, E = TaskBuilderError> = Result<T, E>;
