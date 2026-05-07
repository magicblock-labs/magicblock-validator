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
        CompressedData, TaskInfoFetcher, TaskInfoFetcherError,
        TaskInfoFetcherResult,
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
        accounts: &[(bool, bool, bool, CommittedAccount)],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        let (regular_accounts, compressed_accounts): (Vec<_>, Vec<_>) =
            accounts
                .iter()
                .partition(|(_, _, compressed, _)| !compressed);
        let regular_pubkeys = regular_accounts
            .into_iter()
            .map(|(_, _, _, account)| account.pubkey)
            .collect::<Vec<_>>();
        let compressed_pubkeys = compressed_accounts
            .into_iter()
            .map(|(_, _, _, account)| account.pubkey)
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
        accounts: &[(bool, bool, bool, CommittedAccount)],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
        let diffable_pubkeys = accounts
            .iter()
            .filter(|(_, _, compressed, account)| {
                account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD
                    && !compressed
            })
            .map(|(_, _, _, account)| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .get_base_accounts(&diffable_pubkeys, min_context_slot)
            .await
    }

    async fn fetch_compressed_data<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[(bool, bool, bool, CommittedAccount)],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<
        HashMap<Pubkey, TaskInfoFetcherResult<CompressedData>>,
    > {
        let pubkeys = accounts
            .iter()
            .filter(|(_, _, compressed, _)| *compressed)
            .map(|(_, _, _, account)| account.pubkey)
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
        committed_account: CommittedAccount,
        compressed_data: CompressedData,
    ) -> CommitFinalizeCompressedTask {
        CommitFinalizeCompressedTask {
            commit_id,
            allow_undelegation,
            committed_account,
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
        tasks.extend(Self::create_action_tasks(
            intent_bundle.standalone_actions().as_slice(),
        ));

        let committed_accounts =
            intent_bundle.get_commit_intent_accounts().cloned();
        let undelegated_accounts =
            intent_bundle.get_undelegate_intent_accounts().cloned();
        let commit_finalize_accounts =
            intent_bundle.get_commit_finalize_intent_accounts().cloned();
        let commit_finalize_and_undelegate_accounts = intent_bundle
            .get_commit_finalize_and_undelegate_intent_accounts()
            .cloned();
        let commit_finalize_compressed_accounts = intent_bundle
            .get_commit_finalize_compressed_intent_accounts()
            .cloned();
        let commit_finalize_compressed_and_undelegate_accounts = intent_bundle
            .get_commit_finalize_compressed_and_undelegate_intent_accounts()
            .cloned();

        let flagged_accounts: Vec<_> = [
            (false, false, false, committed_accounts),
            (true, false, false, undelegated_accounts),
            (false, true, false, commit_finalize_accounts),
            (true, true, false, commit_finalize_and_undelegate_accounts),
            (false, true, true, commit_finalize_compressed_accounts),
            (
                true,
                true,
                true,
                commit_finalize_compressed_and_undelegate_accounts,
            ),
        ]
        .into_iter()
        .flat_map(|(allow_undelegation, finalize, compressed, accounts)| {
            accounts.into_iter().flatten().map(move |account| {
                (allow_undelegation, finalize, compressed, account)
            })
        })
        .collect();

        // Get commit nonces and base accounts
        let min_context_slot = flagged_accounts
            .iter()
            .map(|(_, _, _, account)| account.remote_slot)
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
        let commit_ids =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;
        let mut base_accounts = base_accounts.unwrap_or_else(|err| {
                tracing::warn!(intent_id = intent_bundle.id, error = ?err, "Failed to fetch base accounts, falling back to CommitState");
                Default::default()
            });
        let mut compressed_data =
            compressed_data.map_err(TaskBuilderError::CommitTasksBuildError)?;

        // Persist commit ids for commitees
        commit_ids
            .iter()
            .for_each(|(pubkey, commit_id) | {
                if let Err(err) = persister.set_commit_id(intent_bundle.id, pubkey, *commit_id) {
                    error!(intent_id = intent_bundle.id, pubkey = %pubkey, error = ?err, "Failed to persist commit id");
                }
            });

        // Create commit tasks
        let commit_tasks = flagged_accounts
                    .into_iter()
                    .map(
                        |(allow_undelegation, finalize, compressed, account)| -> TaskBuilderResult<BaseTaskImpl> {
                let commit_id = commit_ids
                    .get(&account.pubkey)
                    .copied()
                    .unwrap_or_else(|| {
                        // This shall not ever happen since TaskInfoFetcher
                        // returns commit ids for all pubkeys or throws
                        // If it does occur, it will be patched and retried by IntentExecutor
                        error!(pubkey = %account.pubkey, "Commit id absent for pubkey");
                        0
                    });
                let base_account = base_accounts.remove(&account.pubkey);

                if compressed {
                    let compressed_data = compressed_data
                        .remove(&account.pubkey)
                        .ok_or_else(|| {
                            TaskBuilderError::CommitFinalizeCompressedTasksBuildError(
                                TaskInfoFetcherError::CompressedAccountNotFound(account.pubkey),
                            )
                        })?
                        .map_err(TaskBuilderError::CommitFinalizeCompressedTasksBuildError)?;
                    Ok(Self::create_commit_finalize_compressed_task(
                        commit_id,
                        allow_undelegation,
                        account.clone(),
                        compressed_data,
                    ).into())
                } else if finalize {
                    Ok(Self::create_commit_finalize_task(commit_id, allow_undelegation, account.clone(), base_account).into())
                } else {
                    Ok(Self::create_commit_task(commit_id, allow_undelegation, account.clone(), base_account).into())
                }
            },
        ).collect::<TaskBuilderResult<Vec<_>>>()?;
        tasks.extend(commit_tasks);
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
