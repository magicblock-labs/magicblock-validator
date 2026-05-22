use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures_util::future::try_join_all;
use magicblock_core::intent::CommittedAccount;
use magicblock_program::magic_scheduled_base_intent::{
    BaseAction, CommitAndUndelegate, CommitType, ScheduledIntentBundle,
    UndelegateType,
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::error;

use crate::{
    intent_executor::task_info_fetcher::{
        CompressedData, PhotonFetcherError, TaskInfoFetcher,
        TaskInfoFetcherError, TaskInfoFetcherResult,
    },
    persist::IntentPersister,
    tasks::{
        commit_finalize_compressed_task::CommitFinalizeCompressedTask,
        commit_task::{CommitDelivery, CommitTask},
        BaseActionTask, BaseActionTaskV1, BaseActionTaskV2, BaseTaskImpl,
        CommitFinalizeTask, FinalizeTask, UndelegateTask,
    },
};

#[async_trait]
pub trait TasksBuilder {
    // Creates tasks for commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        base_intent: &ScheduledIntentBundle,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>>;

    // Create tasks for finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledIntentBundle,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>>;
}

/// Necessary info for commit stage task creation
pub struct CommitStageTaskInfo {
    /// commit nonce for a given address
    commit_nonces: HashMap<Pubkey, u64>,
    /// Base account state for diff calculation
    base_accounts: HashMap<Pubkey, Account>,
    /// Data used for compressed accounts
    compressed_data:
        HashMap<Pubkey, Result<CompressedData, TaskInfoFetcherError>>,
}

/// Task builder
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
    ) -> CommitTask {
        let base_account =
            if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
                base_account
            } else {
                None
            };

        let delivery_details = if let Some(base_account) = base_account {
            CommitDelivery::DiffInArgs { base_account }
        } else {
            CommitDelivery::StateInArgs
        };

        CommitTask {
            commit_id,
            allow_undelegation,
            committed_account: account,
            delivery_details,
        }
    }

    fn create_action_tasks<'a>(
        actions: &'a [BaseAction],
    ) -> impl Iterator<Item = BaseTaskImpl> + 'a {
        actions.iter().map(|action| {
            let task = match action.source_program {
                Some(source_program) => BaseActionTask::V2(BaseActionTaskV2 {
                    action: action.clone(),
                    source_program,
                }),
                None => BaseActionTask::V1(BaseActionTaskV1 {
                    action: action.clone(),
                }),
            };
            task.into()
        })
    }

    async fn fetch_commit_nonces<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[(CommittedAccount, bool)],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        let (regular_accounts, compressed_accounts): (Vec<_>, Vec<_>) =
            accounts.iter().partition(|(_, compressed)| !compressed);
        let regular_pubkeys = regular_accounts
            .into_iter()
            .map(|(account, _)| account.pubkey)
            .collect::<Vec<_>>();
        let compressed_pubkeys = compressed_accounts
            .into_iter()
            .map(|(account, _)| account.pubkey)
            .collect::<Vec<_>>();

        let regular_nonces = task_info_fetcher
            .fetch_next_commit_nonces(&regular_pubkeys, false, min_context_slot)
            .await?;
        let compressed_nonces = task_info_fetcher
            .fetch_next_commit_nonces(
                &compressed_pubkeys,
                true,
                min_context_slot,
            )
            .await?;
        Ok(regular_nonces
            .into_iter()
            .chain(compressed_nonces.into_iter())
            .collect::<HashMap<Pubkey, u64>>())
    }

    async fn fetch_diffable_accounts<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[(CommittedAccount, bool)],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
        let diffable_pubkeys = accounts
            .iter()
            .filter(|(account, compressed)| {
                account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD
                    && !compressed
            })
            .map(|(account, _)| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .get_base_accounts(&diffable_pubkeys, min_context_slot)
            .await
    }

    async fn fetch_compressed_data<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[(CommittedAccount, bool)],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<
        HashMap<Pubkey, TaskInfoFetcherResult<CompressedData>>,
    > {
        let pubkeys = accounts
            .iter()
            .filter(|(_, compressed)| *compressed)
            .map(|(account, _)| account.pubkey)
            .collect::<Vec<_>>();
        Ok(pubkeys
            .iter()
            .zip(
                task_info_fetcher
                    .get_compressed_data_for_accounts(
                        &pubkeys,
                        Some(min_context_slot),
                    )
                    .await?,
            )
            .map(|(pk, data)| (*pk, data))
            .collect())
    }

    async fn fetch_commit_stage_info<C: TaskInfoFetcher, P: IntentPersister>(
        intent_bundle: &ScheduledIntentBundle,
        task_info_fetcher: &Arc<C>,
        persister: &Option<P>,
    ) -> TaskBuilderResult<CommitStageTaskInfo> {
        // Fetch necessary data for BaseTasks creation
        let all_committed_accounts = intent_bundle.get_all_committed_accounts();
        let all_regular_committed_accounts =
            intent_bundle.get_all_regular_committed_accounts();
        let all_compressed_committed_accounts =
            intent_bundle.get_all_compressed_committed_accounts();
        let flagged_accounts = all_regular_committed_accounts
            .into_iter()
            .map(|acc| (acc, false))
            .chain(
                all_compressed_committed_accounts
                    .into_iter()
                    .map(|acc| (acc, true)),
            )
            .collect::<Vec<_>>();
        // Get commit nonces and base accounts
        let min_context_slot = all_committed_accounts
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let (commit_ids, base_accounts, compressed_data) = tokio::join!(
            Self::fetch_commit_nonces(
                task_info_fetcher,
                &flagged_accounts,
                min_context_slot
            ),
            Self::fetch_diffable_accounts(
                task_info_fetcher,
                &flagged_accounts,
                min_context_slot
            ),
            Self::fetch_compressed_data(
                task_info_fetcher,
                &flagged_accounts,
                min_context_slot
            )
        );
        let commit_nonces =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;
        let base_accounts = base_accounts.unwrap_or_else(|err| {
            tracing::warn!(intent_id = intent_bundle.id, error = ?err, "Failed to fetch base accounts, falling back to CommitState");
            Default::default()
        });
        let compressed_data =
            compressed_data.map_err(TaskBuilderError::CommitTasksBuildError)?;

        // Persist commit ids for commitees
        commit_nonces
            .iter()
            .for_each(|(pubkey, commit_id) | {
                if let Err(err) = persister.set_commit_id(intent_bundle.id, pubkey, *commit_id) {
                    error!(intent_id = intent_bundle.id, pubkey = %pubkey, error = ?err, "Failed to persist commit id");
                }
            });

        Ok(CommitStageTaskInfo {
            commit_nonces,
            base_accounts,
            compressed_data,
        })
    }

    pub fn create_commit_finalize_task(
        commit_id: u64,
        allow_undelegation: bool,
        account: CommittedAccount,
        base_account: Option<Account>,
    ) -> CommitFinalizeTask {
        let base_account =
            if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
                base_account
            } else {
                None
            };

        let delivery_details = if let Some(base_account) = base_account {
            CommitDelivery::DiffInArgs { base_account }
        } else {
            CommitDelivery::StateInArgs
        };

        CommitFinalizeTask {
            commit_id,
            allow_undelegation,
            committed_account: account,
            delivery: delivery_details,
        }
    }

    pub fn create_commit_finalize_compressed_task(
        commit_id: u64,
        allow_undelegation: bool,
        account: CommittedAccount,
        compressed_data: CompressedData,
    ) -> CommitFinalizeCompressedTask {
        CommitFinalizeCompressedTask {
            commit_id,
            allow_undelegation,
            committed_account: account,
            compressed_data,
        }
    }
}

#[async_trait]
impl TasksBuilder for TaskBuilderImpl {
    /// Returns [`BaseTaskImpl`]s for Commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        task_info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        let mut tasks = Vec::new();
        // Add standalone actions first
        tasks.extend(Self::create_action_tasks(
            intent_bundle.standalone_actions().as_slice(),
        ));

        // Fetch data necessary for task creation
        let CommitStageTaskInfo {
            mut commit_nonces,
            mut base_accounts,
            mut compressed_data,
        } = Self::fetch_commit_stage_info(
            intent_bundle,
            task_info_fetcher,
            persister,
        )
        .await?;

        // Create tasks per intent type
        if let Some(ref value) = intent_bundle.intent_bundle.commit {
            tasks.extend(
                CommitBuilder {
                    commit_nonces: &mut commit_nonces,
                    base_accounts: &mut base_accounts,
                }
                .build(value),
            );
        }
        if let Some(ref value) = intent_bundle.intent_bundle.commit_finalize {
            tasks.extend(
                CommitFinalizeBuilder {
                    commit_nonces: &mut commit_nonces,
                    base_accounts: &mut base_accounts,
                }
                .build(value),
            );
        }
        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_and_undelegate
        {
            tasks.extend(
                CommitAndUndelegateBuilder {
                    commit_nonces: &mut commit_nonces,
                    base_accounts: &mut base_accounts,
                }
                .build(&value.commit_action),
            );
        }
        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_finalize_and_undelegate
        {
            tasks.extend(
                CommitFinalizeAndUndelegateBuilder {
                    commit_nonces: &mut commit_nonces,
                    base_accounts: &mut base_accounts,
                }
                .build(&value.commit_action),
            );
        }
        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_finalize_compressed
        {
            tasks.extend(
                CommitFinalizeCompressedBuilder {
                    commit_nonces: &mut commit_nonces,
                    base_accounts: &mut base_accounts,
                    compressed_data: &mut compressed_data,
                }
                .build(value)?,
            );
        }
        if let Some(ref value) = intent_bundle
            .intent_bundle
            .commit_finalize_compressed_and_undelegate
        {
            tasks.extend(
                CommitFinalizeAndUndelegateCompressedBuilder {
                    commit_nonces: &mut commit_nonces,
                    base_accounts: &mut base_accounts,
                    compressed_data: &mut compressed_data,
                }
                .build(value)?,
            );
        }

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        // Helper to create a finalize task
        fn finalize_task(account: &CommittedAccount) -> BaseTaskImpl {
            FinalizeTask {
                delegated_account: account.pubkey,
            }
            .into()
        }

        // Helper to create an undelegate task
        fn undelegate_task(
            account: &CommittedAccount,
            rent_reimbursement: &Pubkey,
        ) -> BaseTaskImpl {
            UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
            }
            .into()
        }

        // Helper to process commit types
        fn create_finalize_tasks(commit: &CommitType) -> Vec<BaseTaskImpl> {
            match commit {
                CommitType::Standalone(accounts) => {
                    accounts.iter().map(finalize_task).collect()
                }
                CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions,
                } => {
                    let mut tasks = committed_accounts
                        .iter()
                        .map(finalize_task)
                        .collect::<Vec<_>>();
                    tasks.extend(TaskBuilderImpl::create_action_tasks(
                        base_actions,
                    ));
                    tasks
                }
            }
        }

        async fn create_undelegate_tasks<C: TaskInfoFetcher>(
            commit_and_undelegate: &CommitAndUndelegate,
            info_fetcher: &Arc<C>,
        ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
            // Get rent reimbursments for undelegated accounts
            let accounts = commit_and_undelegate.get_committed_accounts();
            let mut min_context_slot = 0;
            let pubkeys = accounts
                .iter()
                .map(|account| {
                    min_context_slot =
                        std::cmp::max(min_context_slot, account.remote_slot);
                    account.pubkey
                })
                .collect::<Vec<_>>();
            let rent_reimbursements = info_fetcher
                .fetch_rent_reimbursements(&pubkeys, min_context_slot)
                .await
                .map_err(TaskBuilderError::FinalizedTasksBuildError)?;

            let mut tasks = accounts
                .iter()
                .zip(rent_reimbursements)
                .map(|(account, rent_reimbursement)| {
                    undelegate_task(account, &rent_reimbursement)
                })
                .collect::<Vec<_>>();

            if let UndelegateType::WithBaseActions(actions) =
                &commit_and_undelegate.undelegate_action
            {
                tasks.extend(TaskBuilderImpl::create_action_tasks(actions));
            }
            Ok(tasks)
        }

        let mut tasks = Vec::new();
        let mut futures = Vec::with_capacity(2);

        if let Some(ref value) = intent_bundle.intent_bundle.commit {
            tasks.extend(create_finalize_tasks(value));
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_and_undelegate
        {
            tasks.extend(create_finalize_tasks(&value.commit_action));
            futures.push(create_undelegate_tasks(value, info_fetcher));
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_finalize_and_undelegate
        {
            futures.push(create_undelegate_tasks(value, info_fetcher));
        }

        tasks.extend(try_join_all(futures).await?.into_iter().flatten());

        Ok(tasks)
    }
}

struct CommitBuilder<'a> {
    commit_nonces: &'a mut HashMap<Pubkey, u64>,
    base_accounts: &'a mut HashMap<Pubkey, Account>,
}

impl<'a> CommitBuilder<'a> {
    fn build(&mut self, commit_type: &CommitType) -> Vec<BaseTaskImpl> {
        commit_type
            .get_committed_accounts()
            .iter()
            .map(|account| {
                let nonce =
                    take_commit_nonce(self.commit_nonces, account.pubkey);
                let base = self.base_accounts.remove(&account.pubkey);
                TaskBuilderImpl::create_commit_task(
                    nonce,
                    false,
                    account.clone(),
                    base,
                )
                .into()
            })
            .collect()
    }
}

struct CommitAndUndelegateBuilder<'a> {
    commit_nonces: &'a mut HashMap<Pubkey, u64>,
    base_accounts: &'a mut HashMap<Pubkey, Account>,
}

impl<'a> CommitAndUndelegateBuilder<'a> {
    fn build(&mut self, commit_type: &CommitType) -> Vec<BaseTaskImpl> {
        commit_type
            .get_committed_accounts()
            .iter()
            .map(|account| {
                let nonce =
                    take_commit_nonce(self.commit_nonces, account.pubkey);
                let base = self.base_accounts.remove(&account.pubkey);
                TaskBuilderImpl::create_commit_task(
                    nonce,
                    true,
                    account.clone(),
                    base,
                )
                .into()
            })
            .collect()
    }
}

struct CommitFinalizeBuilder<'a> {
    commit_nonces: &'a mut HashMap<Pubkey, u64>,
    base_accounts: &'a mut HashMap<Pubkey, Account>,
}

impl<'a> CommitFinalizeBuilder<'a> {
    fn build(&mut self, commit_type: &CommitType) -> Vec<BaseTaskImpl> {
        let mut tasks: Vec<BaseTaskImpl> = commit_type
            .get_committed_accounts()
            .iter()
            .map(|account| {
                let nonce =
                    take_commit_nonce(self.commit_nonces, account.pubkey);
                let base = self.base_accounts.remove(&account.pubkey);
                TaskBuilderImpl::create_commit_finalize_task(
                    nonce,
                    false,
                    account.clone(),
                    base,
                )
                .into()
            })
            .collect();
        if let CommitType::WithBaseActions {
            ref base_actions, ..
        } = commit_type
        {
            tasks.extend(TaskBuilderImpl::create_action_tasks(base_actions));
        }
        tasks
    }
}

struct CommitFinalizeAndUndelegateBuilder<'a> {
    commit_nonces: &'a mut HashMap<Pubkey, u64>,
    base_accounts: &'a mut HashMap<Pubkey, Account>,
}

impl<'a> CommitFinalizeAndUndelegateBuilder<'a> {
    fn build(&mut self, commit_type: &CommitType) -> Vec<BaseTaskImpl> {
        let mut tasks: Vec<BaseTaskImpl> = commit_type
            .get_committed_accounts()
            .iter()
            .map(|account| {
                let nonce =
                    take_commit_nonce(self.commit_nonces, account.pubkey);
                let base = self.base_accounts.remove(&account.pubkey);
                TaskBuilderImpl::create_commit_finalize_task(
                    nonce,
                    true,
                    account.clone(),
                    base,
                )
                .into()
            })
            .collect();
        if let CommitType::WithBaseActions {
            ref base_actions, ..
        } = commit_type
        {
            tasks.extend(TaskBuilderImpl::create_action_tasks(base_actions));
        }
        tasks
    }
}

struct CommitFinalizeCompressedBuilder<'a> {
    commit_nonces: &'a mut HashMap<Pubkey, u64>,
    base_accounts: &'a mut HashMap<Pubkey, Account>,
    compressed_data:
        &'a mut HashMap<Pubkey, Result<CompressedData, TaskInfoFetcherError>>,
}

impl<'a> CommitFinalizeCompressedBuilder<'a> {
    fn build(
        &mut self,
        commit_type: &CommitType,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        let mut tasks: Vec<BaseTaskImpl> = commit_type
            .get_committed_accounts()
            .iter()
            .map(|account| -> TaskBuilderResult<BaseTaskImpl> {
                let nonce =
                    take_commit_nonce(self.commit_nonces, account.pubkey);
                let _base = self.base_accounts.remove(&account.pubkey);
                let compressed_data = self
                    .compressed_data
                    .remove(&account.pubkey)
                    .ok_or(
                        TaskBuilderError::CommitFinalizeCompressedTasksBuildError(
                            PhotonFetcherError::MissingCompressedData.into(),
                        ),
                    )??;
                Ok(
                    TaskBuilderImpl::create_commit_finalize_compressed_task(
                        nonce,
                        false,
                        account.clone(),
                        compressed_data,
                    )
                    .into(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let CommitType::WithBaseActions {
            ref base_actions, ..
        } = commit_type
        {
            tasks.extend(TaskBuilderImpl::create_action_tasks(base_actions));
        }
        Ok(tasks)
    }
}

struct CommitFinalizeAndUndelegateCompressedBuilder<'a> {
    commit_nonces: &'a mut HashMap<Pubkey, u64>,
    base_accounts: &'a mut HashMap<Pubkey, Account>,
    compressed_data:
        &'a mut HashMap<Pubkey, Result<CompressedData, TaskInfoFetcherError>>,
}

impl<'a> CommitFinalizeAndUndelegateCompressedBuilder<'a> {
    fn build(
        &mut self,
        commit_and_undelegate: &CommitAndUndelegate,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        let commit_type = &commit_and_undelegate.commit_action;
        let mut tasks: Vec<BaseTaskImpl> = commit_type
            .get_committed_accounts()
            .iter()
            .map(|account| -> TaskBuilderResult<BaseTaskImpl> {
                let nonce =
                    take_commit_nonce(self.commit_nonces, account.pubkey);
                let _base = self.base_accounts.remove(&account.pubkey);
                let compressed_data = self
                    .compressed_data
                    .remove(&account.pubkey)
                    .ok_or(
                        TaskBuilderError::CommitFinalizeCompressedTasksBuildError(
                            PhotonFetcherError::MissingCompressedData.into(),
                        ),
                    )??;
                Ok(
                    TaskBuilderImpl::create_commit_finalize_compressed_task(
                        nonce,
                        true,
                        account.clone(),
                        compressed_data,
                    )
                    .into(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let CommitType::WithBaseActions {
            ref base_actions, ..
        } = commit_type
        {
            tasks.extend(TaskBuilderImpl::create_action_tasks(base_actions));
        }
        Ok(tasks)
    }
}

fn take_commit_nonce(
    commit_nonces: &mut HashMap<Pubkey, u64>,
    pubkey: Pubkey,
) -> u64 {
    commit_nonces.remove(&pubkey).unwrap_or_else(|| {
        // This shall not ever happen since TaskInfoFetcher
        // returns commit ids for all pubkeys or throws
        // If it does occur, it will be patched and retried by IntentExecutor
        error!(pubkey = %pubkey, "Commit id absent for pubkey");
        0
    })
}

#[derive(thiserror::Error, Debug)]
pub enum TaskBuilderError {
    #[error("CommitIdFetchError: {0}")]
    CommitTasksBuildError(#[source] TaskInfoFetcherError),
    #[error("FinalizedTasksBuildError: {0}")]
    FinalizedTasksBuildError(#[source] TaskInfoFetcherError),
    #[error("CommitFinalizeCompressedTasksBuildError: {0}")]
    CommitFinalizeCompressedTasksBuildError(#[from] TaskInfoFetcherError),
}

impl TaskBuilderError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::CommitTasksBuildError(err) => err.signature(),
            Self::FinalizedTasksBuildError(err) => err.signature(),
            Self::CommitFinalizeCompressedTasksBuildError(_) => None,
        }
    }
}

pub type TaskBuilderResult<T, E = TaskBuilderError> = Result<T, E>;
