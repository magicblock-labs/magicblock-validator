use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
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
        BaseActionTask, BaseTask, CompressedCommitTask, FinalizeTask,
        UndelegateTask,
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
        compressed_data: Option<&CompressedData>,
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

        if let Some(compressed_data) = compressed_data {
            ArgsTaskType::CompressedCommitAndFinalize(CompressedCommitTask {
                commit_id,
                allow_undelegation,
                committed_account: account.clone(),
                compressed_data: compressed_data.clone(),
            })
        } else if let Some(base_account) = base_account {
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

        let (commit_ids, base_accounts, associated_compressed_data) = {
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
                ),
                async {
                    if compressed {
                        let pks = accounts
                            .iter()
                            .map(|account| account.pubkey)
                            .collect::<Vec<_>>();
                        Ok(info_fetcher
                            .get_compressed_data_for_accounts(
                                &pks,
                                Some(min_context_slot),
                            )
                            .await?
                            .iter()
                            .zip(pks)
                            .filter_map(|(data, pk)| {
                                data.as_ref().map(|data| (pk, data.clone()))
                            })
                            .collect::<HashMap<_, _>>())
                    } else {
                        Ok(HashMap::new())
                    }
                }
            )
        };

        let commit_ids =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;
        let associated_compressed_data = associated_compressed_data
            .map_err(TaskBuilderError::CommitTasksBuildError)?;

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

        let tasks = accounts
            .iter()
            .map(|account| {
                let commit_id = *commit_ids
                    .get(&account.pubkey)
                    .ok_or(TaskBuilderError::MissingCommitId(account.pubkey))?;
                let compressed_data = if compressed {
                    Some(
                        associated_compressed_data.get(&account.pubkey).ok_or(
                            TaskBuilderError::MissingCompressedData(
                                account.pubkey,
                            ),
                        )?,
                    )
                } else {
                    None
                };

                // TODO (snawaz): if accounts do not have duplicate, then we can use remove
                // instead:
                //  let base_account = base_accounts.remove(&account.pubkey);
                let base_account = base_accounts.get(&account.pubkey).cloned();
                let task = Self::create_commit_task(
                    commit_id,
                    compressed_data,
                    allow_undelegation,
                    account.clone(),
                    base_account,
                );
                Ok::<_, TaskBuilderError>(Box::new(task) as Box<dyn BaseTask>)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        // Helper to create a finalize task
        fn finalize_task(account: &CommittedAccount) -> Box<dyn BaseTask> {
            let task_type = ArgsTaskType::Finalize(FinalizeTask {
                delegated_account: account.pubkey,
            });
            Box::new(ArgsTask::new(task_type))
        }

        // Helper to create an undelegate task
        fn undelegate_task(
            account: &CommittedAccount,
            rent_reimbursement: &Pubkey,
        ) -> Box<dyn BaseTask> {
            let task_type = ArgsTaskType::Undelegate(UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
            });
            Box::new(ArgsTask::new(task_type))
        }

        // Helper to process commit types
        fn process_commit(
            commit: &CommitType,
        ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
            match commit {
                CommitType::Standalone(committed_accounts) => {
                    Ok(committed_accounts
                        .iter()
                        .map(|account| finalize_task(account))
                        .collect())
                }
                CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions,
                    ..
                } => {
                    let mut tasks = committed_accounts
                        .iter()
                        .map(|account| finalize_task(account))
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

        match &base_intent.base_intent {
            MagicBaseIntent::BaseActions(_) => Ok(vec![]),
            MagicBaseIntent::Commit(commit)
            | MagicBaseIntent::CompressedCommit(commit) => {
                Ok(process_commit(commit)?)
            }
            MagicBaseIntent::CommitAndUndelegate(t) => {
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
                let mut tasks = process_commit(&t.commit_action)?;
                let rent_reimbursements = info_fetcher
                    .fetch_rent_reimbursements(&pubkeys, min_context_slot)
                    .await
                    .map_err(TaskBuilderError::FinalizedTasksBuildError)?;

                tasks.extend(accounts.iter().zip(rent_reimbursements).map(
                    |(account, rent_reimbursement)| {
                        undelegate_task(account, &rent_reimbursement)
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
                let mut tasks = process_commit(&t.commit_action)?;

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
    #[error("MissingCompressedData: {0}")]
    MissingCompressedData(Pubkey),
}

impl TaskBuilderError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::CommitTasksBuildError(err) => err.signature(),
            Self::FinalizedTasksBuildError(err) => err.signature(),
            Self::TaskStrategistError(_) => None,
            Self::MissingCommitId(_) => None,
            Self::TaskInfoFetcherError(err) => err.signature(),
            Self::MissingCompressedData(_) => None,
        }
    }
}

pub type TaskBuilderResult<T, E = TaskBuilderError> = Result<T, E>;
