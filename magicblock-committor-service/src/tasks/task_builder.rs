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
        TaskInfoFetcher, TaskInfoFetcherError, TaskInfoFetcherResult,
    },
    persist::IntentPersister,
    tasks::{
        commit_task::{CommitDelivery, CommitTask},
        BaseActionTask, BaseActionTaskV1, BaseActionTaskV2, BaseTaskImpl,
        CommitFinalizeTask, FinalizeTask, RentPendingAtaTask, UndelegateTask,
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
        missing_metadata_as_zero: &[Pubkey],
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        let committed_pubkeys = accounts
            .iter()
            .map(|account| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .fetch_next_commit_nonces_with_missing_as_zero(
                &committed_pubkeys,
                min_context_slot,
                missing_metadata_as_zero,
            )
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

    async fn fetch_commit_stage_info<C: TaskInfoFetcher, P: IntentPersister>(
        intent_bundle: &ScheduledIntentBundle,
        task_info_fetcher: &Arc<C>,
        persister: &Option<P>,
    ) -> TaskBuilderResult<CommitStageTaskInfo> {
        // Fetch necessary data for BaseTasks creation
        let rent_pending_pubkeys = intent_bundle
            .intent_bundle
            .rent_pending_ata_materializations
            .iter()
            .map(|materialization| materialization.eata_pubkey)
            .collect::<Vec<_>>();
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
                min_context_slot,
                &rent_pending_pubkeys
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

        tasks.extend(
            intent_bundle
                .intent_bundle
                .rent_pending_ata_materializations
                .iter()
                .flat_map(|materialization| {
                    let task = RentPendingAtaTask {
                        materialization: materialization.clone(),
                    };
                    [
                        BaseTaskImpl::InitializeRentPendingAta(task.clone()),
                        BaseTaskImpl::DelegateRentPendingAta(task),
                    ]
                }),
        );

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
            rent_pending_reimbursements: &HashMap<Pubkey, Pubkey>,
        ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
            // Get rent reimbursments for undelegated accounts
            let accounts = commit_and_undelegate.get_committed_accounts();
            let mut min_context_slot = 0;
            let pubkeys = accounts
                .iter()
                .filter_map(|account| {
                    min_context_slot =
                        std::cmp::max(min_context_slot, account.remote_slot);
                    (!rent_pending_reimbursements.contains_key(&account.pubkey))
                        .then_some(account.pubkey)
                })
                .collect::<Vec<_>>();
            let fetched_rent_reimbursements = info_fetcher
                .fetch_rent_reimbursements(&pubkeys, min_context_slot)
                .await
                .map_err(TaskBuilderError::FinalizedTasksBuildError)?;
            let rent_reimbursements = pubkeys
                .into_iter()
                .zip(fetched_rent_reimbursements)
                .collect::<HashMap<_, _>>();

            let mut tasks = Vec::with_capacity(accounts.len());
            for account in accounts {
                let rent_reimbursement = if let Some(rent_reimbursement) =
                    rent_pending_reimbursements.get(&account.pubkey)
                {
                    *rent_reimbursement
                } else if let Some(rent_reimbursement) =
                    rent_reimbursements.get(&account.pubkey)
                {
                    *rent_reimbursement
                } else {
                    return Err(TaskBuilderError::FinalizedTasksBuildError(
                        TaskInfoFetcherError::AccountNotFoundError(
                            account.pubkey,
                        ),
                    ));
                };
                tasks.push(undelegate_task(account, &rent_reimbursement));
            }

            if let UndelegateType::WithBaseActions(actions) =
                &commit_and_undelegate.undelegate_action
            {
                tasks.extend(TaskBuilderImpl::create_action_tasks(actions));
            }
            Ok(tasks)
        }

        let mut tasks = Vec::new();
        let rent_pending_reimbursements = intent_bundle
            .intent_bundle
            .rent_pending_ata_materializations
            .iter()
            .map(|materialization| {
                (materialization.eata_pubkey, materialization.validator)
            })
            .collect::<HashMap<_, _>>();
        let mut futures = Vec::with_capacity(2);

        if let Some(ref value) = intent_bundle.intent_bundle.commit {
            tasks.extend(create_finalize_tasks(value));
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_and_undelegate
        {
            tasks.extend(create_finalize_tasks(&value.commit_action));
            futures.push(create_undelegate_tasks(
                value,
                info_fetcher,
                &rent_pending_reimbursements,
            ));
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_finalize_and_undelegate
        {
            futures.push(create_undelegate_tasks(
                value,
                info_fetcher,
                &rent_pending_reimbursements,
            ));
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
}

impl TaskBuilderError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::CommitTasksBuildError(err) => err.signature(),
            Self::FinalizedTasksBuildError(err) => err.signature(),
        }
    }
}

pub type TaskBuilderResult<T, E = TaskBuilderError> = Result<T, E>;

#[cfg(test)]
mod tests {
    use magicblock_core::token_programs::{
        EphemeralAta, RentPendingAtaMaterialization, EATA_PROGRAM_ID,
        TOKEN_PROGRAM_ID,
    };
    use magicblock_program::magic_scheduled_base_intent::{
        MagicIntentBundle, ScheduledIntentBundle,
    };
    use solana_hash::Hash;
    use solana_transaction::Transaction;

    use super::*;
    use crate::tasks::BaseTask;

    struct EmptyFetcher;

    #[async_trait]
    impl TaskInfoFetcher for EmptyFetcher {
        async fn fetch_next_commit_nonces(
            &self,
            pubkeys: &[Pubkey],
            min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            self.fetch_next_commit_nonces_with_missing_as_zero(
                pubkeys,
                min_context_slot,
                pubkeys,
            )
            .await
        }

        async fn fetch_next_commit_nonces_with_missing_as_zero(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
            missing_metadata_as_zero: &[Pubkey],
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            assert_eq!(pubkeys, missing_metadata_as_zero);
            Ok(pubkeys.iter().map(|pubkey| (*pubkey, 1)).collect())
        }

        async fn fetch_current_commit_nonces(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            assert!(pubkeys.is_empty());
            Ok(HashMap::new())
        }

        async fn fetch_rent_reimbursements(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
            assert!(pubkeys.is_empty());
            Ok(Vec::new())
        }

        async fn get_base_accounts(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
            assert!(pubkeys.is_empty());
            Ok(HashMap::new())
        }
    }

    struct ExistingMetadataFetcher {
        next_nonce: u64,
    }

    #[async_trait]
    impl TaskInfoFetcher for ExistingMetadataFetcher {
        async fn fetch_next_commit_nonces(
            &self,
            pubkeys: &[Pubkey],
            min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            self.fetch_next_commit_nonces_with_missing_as_zero(
                pubkeys,
                min_context_slot,
                &[],
            )
            .await
        }

        async fn fetch_next_commit_nonces_with_missing_as_zero(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
            missing_metadata_as_zero: &[Pubkey],
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            assert_eq!(pubkeys, missing_metadata_as_zero);
            Ok(pubkeys
                .iter()
                .map(|pubkey| (*pubkey, self.next_nonce))
                .collect())
        }

        async fn fetch_current_commit_nonces(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(pubkeys
                .iter()
                .map(|pubkey| (*pubkey, self.next_nonce.saturating_sub(1)))
                .collect())
        }

        async fn fetch_rent_reimbursements(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
            assert!(pubkeys.is_empty());
            Ok(Vec::new())
        }

        async fn get_base_accounts(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
            assert!(pubkeys.is_empty());
            Ok(HashMap::new())
        }
    }

    fn rent_pending_intent(
        commit_and_undelegate: bool,
    ) -> (ScheduledIntentBundle, Pubkey) {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let eata_pubkey = Pubkey::new_unique();
        let validator = Pubkey::new_unique();
        let eata = EphemeralAta {
            owner: wallet_owner,
            mint,
            amount: 7,
            bump: 255,
        };
        let committed_account = CommittedAccount {
            pubkey: eata_pubkey,
            account: eata.into(),
            remote_slot: 0,
        };
        let materialization = RentPendingAtaMaterialization {
            ata_pubkey: Pubkey::new_unique(),
            eata_pubkey,
            token_program: TOKEN_PROGRAM_ID,
            wallet_owner,
            mint,
            token_account_data_len: 165,
            validator,
            delegated_payer: Pubkey::new_unique(),
            delegated_vault: Pubkey::new_unique(),
        };
        let intent_bundle = if commit_and_undelegate {
            MagicIntentBundle {
                commit_and_undelegate: Some(CommitAndUndelegate {
                    commit_action: CommitType::Standalone(vec![
                        committed_account,
                    ]),
                    undelegate_action: UndelegateType::Standalone,
                }),
                rent_pending_ata_materializations: vec![materialization],
                ..Default::default()
            }
        } else {
            MagicIntentBundle {
                commit: Some(CommitType::Standalone(vec![committed_account])),
                rent_pending_ata_materializations: vec![materialization],
                ..Default::default()
            }
        };

        (
            ScheduledIntentBundle {
                id: 42,
                slot: 0,
                blockhash: Hash::default(),
                sent_transaction: Transaction::default(),
                payer: Pubkey::new_unique(),
                intent_bundle,
            },
            validator,
        )
    }

    #[tokio::test]
    async fn rent_pending_commit_prepends_eata_materialization() {
        let fetcher = Arc::new(EmptyFetcher);
        let (intent, validator) = rent_pending_intent(false);

        let tasks = TaskBuilderImpl::commit_tasks(
            &fetcher,
            &intent,
            &None::<crate::persist::IntentPersisterImpl>,
        )
        .await
        .unwrap();

        assert!(matches!(
            tasks.as_slice(),
            [
                BaseTaskImpl::InitializeRentPendingAta(_),
                BaseTaskImpl::DelegateRentPendingAta(_),
                BaseTaskImpl::Commit(_)
            ]
        ));
        let delegate_ix = tasks[1].instruction(&validator);
        assert_eq!(delegate_ix.program_id, EATA_PROGRAM_ID);
        assert_eq!(delegate_ix.data[0], 4);
        assert_eq!(&delegate_ix.data[1..], validator.as_ref());
        let BaseTaskImpl::Commit(commit) = &tasks[2] else {
            panic!("expected commit task");
        };
        assert_eq!(commit.commit_id, 1);
    }

    #[tokio::test]
    async fn rent_pending_undelegation_prepends_validator_specific_eata_delegate(
    ) {
        let fetcher = Arc::new(EmptyFetcher);
        let (intent, validator) = rent_pending_intent(true);

        let tasks = TaskBuilderImpl::commit_tasks(
            &fetcher,
            &intent,
            &None::<crate::persist::IntentPersisterImpl>,
        )
        .await
        .unwrap();

        assert!(matches!(
            tasks.as_slice(),
            [
                BaseTaskImpl::InitializeRentPendingAta(_),
                BaseTaskImpl::DelegateRentPendingAta(_),
                BaseTaskImpl::Commit(_)
            ]
        ));
        let delegate_ix = tasks[1].instruction(&validator);
        assert_eq!(delegate_ix.data[0], 4);
        assert_eq!(&delegate_ix.data[1..], validator.as_ref());
    }

    #[tokio::test]
    async fn rent_pending_commit_and_undelegate_finalize_defaults_reimbursement(
    ) {
        let fetcher = Arc::new(EmptyFetcher);
        let (intent, validator) = rent_pending_intent(true);

        let tasks = TaskBuilderImpl::finalize_tasks(&fetcher, &intent)
            .await
            .unwrap();

        assert!(matches!(
            tasks.as_slice(),
            [BaseTaskImpl::Finalize(_), BaseTaskImpl::Undelegate(_)]
        ));
        let BaseTaskImpl::Undelegate(undelegate) = &tasks[1] else {
            panic!("expected undelegate task");
        };
        assert_eq!(undelegate.rent_reimbursement, validator);
    }

    #[tokio::test]
    async fn rent_pending_second_commit_uses_fetched_nonce() {
        let fetcher = Arc::new(ExistingMetadataFetcher { next_nonce: 2 });
        let (intent, _validator) = rent_pending_intent(false);

        let tasks = TaskBuilderImpl::commit_tasks(
            &fetcher,
            &intent,
            &None::<crate::persist::IntentPersisterImpl>,
        )
        .await
        .unwrap();

        let BaseTaskImpl::Commit(commit) = &tasks[2] else {
            panic!("expected commit task");
        };
        assert_eq!(commit.commit_id, 2);
    }

    #[tokio::test]
    async fn rent_pending_conflicting_base_delegation_is_gated_by_eata_delegate(
    ) {
        let fetcher = Arc::new(EmptyFetcher);

        for commit_and_undelegate in [false, true] {
            let (intent, validator) =
                rent_pending_intent(commit_and_undelegate);
            let eata_pubkey =
                intent.intent_bundle.rent_pending_ata_materializations[0]
                    .eata_pubkey;

            let tasks = TaskBuilderImpl::commit_tasks(
                &fetcher,
                &intent,
                &None::<crate::persist::IntentPersisterImpl>,
            )
            .await
            .unwrap();

            // If base creates and delegates the eATA to another validator after
            // local rent-pending creation, e-token delegation is the failing
            // validator-mismatch gate and must precede DLP commit work.
            assert!(matches!(
                tasks.as_slice(),
                [
                    BaseTaskImpl::InitializeRentPendingAta(_),
                    BaseTaskImpl::DelegateRentPendingAta(_),
                    BaseTaskImpl::Commit(_)
                ]
            ));
            let delegate_ix = tasks[1].instruction(&validator);
            assert_eq!(delegate_ix.program_id, EATA_PROGRAM_ID);
            assert_eq!(delegate_ix.accounts[0].pubkey, validator);
            assert!(delegate_ix.accounts[0].is_signer);
            assert_eq!(delegate_ix.accounts[1].pubkey, eata_pubkey);
            assert_eq!(delegate_ix.data[0], 4);
            assert_eq!(&delegate_ix.data[1..], validator.as_ref());
        }
    }
}
