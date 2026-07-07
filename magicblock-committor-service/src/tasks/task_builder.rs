use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use dlp_api::state::{DelegationMetadata, UndelegationRequester};
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
        TaskInfoFetcher, TaskInfoFetcherError, TaskInfoFetcherResult,
    },
    persist::IntentPersister,
    tasks::{
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
        accounts: &[CommittedAccount],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        let committed_pubkeys = accounts
            .iter()
            .map(|account| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .fetch_next_commit_nonces(&committed_pubkeys, min_context_slot)
            .await
    }

    async fn fetch_diffable_accounts<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[CommittedAccount],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
        let diffable_pubkeys = accounts
            .iter()
            .filter(|account| {
                account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD
            })
            .map(|account| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .get_base_accounts(&diffable_pubkeys, min_context_slot)
            .await
    }

    fn get_finalize_stage_metadata_pubkeys(
        intent_bundle: &ScheduledIntentBundle,
    ) -> Vec<Pubkey> {
        [
            intent_bundle.get_undelegate_intent_pubkeys(),
            intent_bundle
                .intent_bundle
                .get_commit_finalize_and_undelegate_intent_pubkeys(),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .collect()
    }

    async fn fetch_commit_stage_info<C: TaskInfoFetcher, P: IntentPersister>(
        intent_bundle: &ScheduledIntentBundle,
        task_info_fetcher: &Arc<C>,
        persister: &Option<P>,
    ) -> TaskBuilderResult<CommitStageTaskInfo> {
        // Fetch necessary data for BaseTasks creation
        let all_committed_accounts = intent_bundle.get_all_committed_accounts();
        // Get commit nonces and base accounts
        let min_context_slot = all_committed_accounts
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let (commit_ids, base_accounts) = tokio::join!(
            Self::fetch_commit_nonces(
                task_info_fetcher,
                &all_committed_accounts,
                min_context_slot
            ),
            Self::fetch_diffable_accounts(
                task_info_fetcher,
                &all_committed_accounts,
                min_context_slot
            )
        );
        let commit_nonces =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;
        let base_accounts = base_accounts.unwrap_or_else(|err| {
            tracing::warn!(intent_id = intent_bundle.id, error = ?err, "Failed to fetch base accounts, falling back to CommitState");
            Default::default()
        });

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
            include_undelegation_request: bool,
        ) -> BaseTaskImpl {
            UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
                include_undelegation_request,
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

        fn create_undelegate_tasks(
            commit_and_undelegate: &CommitAndUndelegate,
            delegation_metadata: &HashMap<Pubkey, DelegationMetadata>,
        ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
            let accounts = commit_and_undelegate.get_committed_accounts();

            let mut tasks = accounts
                .iter()
                .map(|account| {
                    let rent_reimbursement = rent_reimbursement(
                        delegation_metadata,
                        account.pubkey,
                    )?;
                    let include_undelegation_request =
                        should_include_undelegation_request(
                            delegation_metadata,
                            account.pubkey,
                        )?;
                    Ok(undelegate_task(
                        account,
                        &rent_reimbursement,
                        include_undelegation_request,
                    ))
                })
                .collect::<TaskBuilderResult<Vec<_>>>()?;

            if let UndelegateType::WithBaseActions(actions) =
                &commit_and_undelegate.undelegate_action
            {
                tasks.extend(TaskBuilderImpl::create_action_tasks(actions));
            }
            Ok(tasks)
        }

        let mut tasks = Vec::new();
        let finalize_metadata_pubkeys =
            Self::get_finalize_stage_metadata_pubkeys(intent_bundle);
        let min_context_slot = intent_bundle
            .get_all_committed_accounts()
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let delegation_metadata = info_fetcher
            .fetch_delegation_metadata(
                &finalize_metadata_pubkeys,
                min_context_slot,
            )
            .await
            .map_err(TaskBuilderError::FinalizedTasksBuildError)?;

        if let Some(ref value) = intent_bundle.intent_bundle.commit {
            tasks.extend(create_finalize_tasks(value));
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_and_undelegate
        {
            tasks.extend(create_finalize_tasks(&value.commit_action));
            tasks.extend(create_undelegate_tasks(value, &delegation_metadata)?);
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_finalize_and_undelegate
        {
            tasks.extend(create_undelegate_tasks(value, &delegation_metadata)?);
        }

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

fn rent_reimbursement(
    delegation_metadata: &HashMap<Pubkey, DelegationMetadata>,
    pubkey: Pubkey,
) -> TaskBuilderResult<Pubkey> {
    delegation_metadata
        .get(&pubkey)
        .map(|metadata| metadata.rent_payer)
        .ok_or(TaskBuilderError::MissingDelegationMetadata(pubkey))
}

fn should_include_undelegation_request(
    delegation_metadata: &HashMap<Pubkey, DelegationMetadata>,
    pubkey: Pubkey,
) -> TaskBuilderResult<bool> {
    let metadata = delegation_metadata
        .get(&pubkey)
        .ok_or(TaskBuilderError::MissingDelegationMetadata(pubkey))?;
    Ok(metadata.undelegation_requester == UndelegationRequester::OwnerProgram)
}

#[derive(thiserror::Error, Debug)]
pub enum TaskBuilderError {
    #[error("CommitIdFetchError: {0}")]
    CommitTasksBuildError(#[source] TaskInfoFetcherError),
    #[error("FinalizedTasksBuildError: {0}")]
    FinalizedTasksBuildError(#[source] TaskInfoFetcherError),
    #[error("Missing delegation metadata for {0}")]
    MissingDelegationMetadata(Pubkey),
}

impl TaskBuilderError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::CommitTasksBuildError(err) => err.signature(),
            Self::FinalizedTasksBuildError(err) => err.signature(),
            Self::MissingDelegationMetadata(_) => None,
        }
    }
}

pub type TaskBuilderResult<T, E = TaskBuilderError> = Result<T, E>;
