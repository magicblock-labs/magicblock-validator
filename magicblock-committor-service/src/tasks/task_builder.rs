use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use dlp_api::state::{DelegationMetadata, UndelegationRequester};
use magicblock_core::{
    intent::{
        types::CommittedAccount, CommitAndUndelegate, CommitType,
        UndelegateType,
    },
    token_programs::RentPendingAtaMaterialization,
};
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::error;

use crate::{
    intent_executor::task_info_fetcher::{
        CommitNonceFetchResult, TaskInfoFetcher, TaskInfoFetcherError,
        TaskInfoFetcherResult,
    },
    persist::IntentPersister,
    tasks::{
        utils::{
            create_action_tasks, create_commit_finalize_task,
            create_commit_task, COMMIT_STATE_SIZE_THRESHOLD,
        },
        BaseTaskImpl, FinalizeTask, RentPendingAtaTask, UndelegateTask,
    },
};

#[async_trait]
pub trait TasksBuilder {
    // Creates tasks for commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        base_intent: &ScheduledIntentBundle,
        validator: &Pubkey,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>>;

    // Create tasks for finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledIntentBundle,
        validator: &Pubkey,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>>;
}

/// Necessary info for commit stage task creation
pub struct CommitStageTaskInfo {
    /// commit nonce for a given address
    commit_nonces: HashMap<Pubkey, u64>,
    /// Base account state for diff calculation
    base_accounts: HashMap<Pubkey, Account>,
    /// Rent-pending eATAs that need same-transaction materialization
    rent_pending_ata_materializations: Vec<RentPendingAtaMaterialization>,
}

/// Task builder
pub struct TaskBuilderImpl;

impl TaskBuilderImpl {
    async fn fetch_commit_nonces<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[CommittedAccount],
        min_context_slot: u64,
        missing_metadata_as_zero: &[Pubkey],
    ) -> TaskInfoFetcherResult<CommitNonceFetchResult> {
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

    pub(super) fn rent_pending_materialization_tasks(
        materializations: impl IntoIterator<Item = RentPendingAtaMaterialization>,
    ) -> Vec<BaseTaskImpl> {
        let mut seen = HashSet::new();
        materializations
            .into_iter()
            .filter(|materialization| seen.insert(materialization.eata_pubkey))
            .flat_map(|materialization| {
                let task = RentPendingAtaTask { materialization };
                [
                    BaseTaskImpl::InitializeRentPendingAta(task.clone()),
                    BaseTaskImpl::DelegateRentPendingAta(task),
                ]
            })
            .collect()
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
        let all_committed_accounts = intent_bundle.get_all_committed_accounts();
        let rent_pending_pubkeys = intent_bundle
            .intent_bundle
            .rent_pending_ata_materializations
            .iter()
            .map(|materialization| materialization.eata_pubkey)
            .collect::<Vec<_>>();

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
        let commit_nonce_result =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;
        let rent_pending_ata_materializations = intent_bundle
            .intent_bundle
            .rent_pending_ata_materializations
            .clone();
        let commit_nonces = commit_nonce_result.nonces;
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
            rent_pending_ata_materializations,
        })
    }
}

#[async_trait]
impl TasksBuilder for TaskBuilderImpl {
    /// Returns [`BaseTaskImpl`]s for Commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        task_info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
        _validator: &Pubkey,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        let mut tasks = Vec::new();
        // Add standalone actions first
        tasks.extend(create_action_tasks(
            intent_bundle.standalone_actions().as_slice(),
        ));

        // Fetch data necessary for task creation
        let CommitStageTaskInfo {
            mut commit_nonces,
            mut base_accounts,
            rent_pending_ata_materializations,
        } = Self::fetch_commit_stage_info(
            intent_bundle,
            task_info_fetcher,
            persister,
        )
        .await?;

        tasks.extend(Self::rent_pending_materialization_tasks(
            rent_pending_ata_materializations,
        ));

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
        _validator: &Pubkey,
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
                    tasks.extend(create_action_tasks(base_actions));
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
                    let metadata = delegation_metadata_for_pubkey(
                        account.pubkey,
                        delegation_metadata,
                    )?;
                    Ok(undelegate_task(
                        account,
                        &metadata.rent_payer,
                        metadata.undelegation_requester
                            == UndelegationRequester::OwnerProgram,
                    ))
                })
                .collect::<TaskBuilderResult<Vec<_>>>()?;

            if let UndelegateType::WithBaseActions(actions) =
                &commit_and_undelegate.undelegate_action
            {
                tasks.extend(create_action_tasks(actions));
            }
            Ok(tasks)
        }

        let mut tasks = Vec::new();
        let metadata_fallbacks = intent_bundle
            .intent_bundle
            .rent_pending_ata_materializations
            .iter()
            .map(|materialization| {
                (
                    materialization.eata_pubkey,
                    DelegationMetadata {
                        last_commit_id: 0,
                        undelegation_requester: UndelegationRequester::None,
                        seeds: vec![],
                        rent_payer: materialization.validator,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let finalize_metadata_pubkeys =
            Self::get_finalize_stage_metadata_pubkeys(intent_bundle);
        let min_context_slot = intent_bundle
            .get_all_committed_accounts()
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let delegation_metadata = info_fetcher
            .fetch_delegation_metadata_with_fallbacks(
                &finalize_metadata_pubkeys,
                min_context_slot,
                metadata_fallbacks,
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
                create_commit_task(nonce, false, account.clone(), base).into()
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
                create_commit_task(nonce, true, account.clone(), base).into()
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
                create_commit_finalize_task(nonce, false, account.clone(), base)
                    .into()
            })
            .collect();
        if let CommitType::WithBaseActions {
            ref base_actions, ..
        } = commit_type
        {
            tasks.extend(create_action_tasks(base_actions));
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
                create_commit_finalize_task(nonce, true, account.clone(), base)
                    .into()
            })
            .collect();
        if let CommitType::WithBaseActions {
            ref base_actions, ..
        } = commit_type
        {
            tasks.extend(create_action_tasks(base_actions));
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

fn delegation_metadata_for_pubkey(
    pubkey: Pubkey,
    delegation_metadata: &HashMap<Pubkey, DelegationMetadata>,
) -> TaskBuilderResult<&DelegationMetadata> {
    delegation_metadata
        .get(&pubkey)
        .ok_or(TaskBuilderError::MissingDelegationMetadata(pubkey))
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

#[cfg(test)]
mod tests {
    use magicblock_core::token_programs::{
        try_derive_eata_address_and_bump, EphemeralAta,
        RentPendingAtaMaterialization, EATA_PROGRAM_ID, TOKEN_PROGRAM_ID,
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
            .map(|result| result.nonces)
        }

        async fn fetch_next_commit_nonces_with_missing_as_zero(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
            missing_metadata_as_zero: &[Pubkey],
        ) -> TaskInfoFetcherResult<CommitNonceFetchResult> {
            let allowed = missing_metadata_as_zero
                .iter()
                .copied()
                .collect::<HashSet<_>>();
            let mut nonces = HashMap::new();
            let mut missing_metadata = HashSet::new();
            for pubkey in pubkeys {
                if !allowed.contains(pubkey) {
                    return Err(TaskInfoFetcherError::AccountNotFoundError(
                        *pubkey,
                    ));
                }
                nonces.insert(*pubkey, 1);
                missing_metadata.insert(*pubkey);
            }
            Ok(CommitNonceFetchResult {
                nonces,
                missing_metadata,
            })
        }

        async fn fetch_current_commit_nonces(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            assert!(pubkeys.is_empty());
            Ok(HashMap::new())
        }

        async fn fetch_delegation_metadata(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, DelegationMetadata>>
        {
            if let Some(pubkey) = pubkeys.first() {
                Err(TaskInfoFetcherError::AccountNotFoundError(*pubkey))
            } else {
                Ok(HashMap::new())
            }
        }

        async fn fetch_delegation_metadata_with_fallbacks(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
            mut metadata_fallbacks: HashMap<Pubkey, DelegationMetadata>,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, DelegationMetadata>>
        {
            pubkeys
                .iter()
                .map(|pubkey| {
                    metadata_fallbacks
                        .remove(pubkey)
                        .map(|metadata| (*pubkey, metadata))
                        .ok_or(TaskInfoFetcherError::AccountNotFoundError(
                            *pubkey,
                        ))
                })
                .collect()
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

    fn existing_metadata_rent_reimbursement() -> Pubkey {
        Pubkey::new_from_array([7; 32])
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
            .map(|result| result.nonces)
        }

        async fn fetch_next_commit_nonces_with_missing_as_zero(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
            _missing_metadata_as_zero: &[Pubkey],
        ) -> TaskInfoFetcherResult<CommitNonceFetchResult> {
            Ok(CommitNonceFetchResult::from_nonces(
                pubkeys
                    .iter()
                    .map(|pubkey| (*pubkey, self.next_nonce))
                    .collect(),
            ))
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

        async fn fetch_delegation_metadata(
            &self,
            pubkeys: &[Pubkey],
            _min_context_slot: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, DelegationMetadata>>
        {
            Ok(pubkeys
                .iter()
                .map(|pubkey| {
                    (
                        *pubkey,
                        DelegationMetadata {
                            last_commit_id: self.next_nonce.saturating_sub(1),
                            undelegation_requester: UndelegationRequester::None,
                            seeds: vec![],
                            rent_payer: existing_metadata_rent_reimbursement(),
                        },
                    )
                })
                .collect())
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

    fn eata_intent_without_materialization_metadata(
        commit_and_undelegate: bool,
    ) -> (ScheduledIntentBundle, Pubkey) {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let validator = Pubkey::new_unique();
        let (eata_pubkey, bump) =
            try_derive_eata_address_and_bump(&wallet_owner, &mint)
                .expect("eATA PDA should derive");
        let eata = EphemeralAta {
            owner: wallet_owner,
            mint,
            amount: 7,
            bump,
        };
        let committed_account = CommittedAccount {
            pubkey: eata_pubkey,
            account: eata.into(),
            remote_slot: 0,
        };
        let intent_bundle = if commit_and_undelegate {
            MagicIntentBundle {
                commit_and_undelegate: Some(CommitAndUndelegate {
                    commit_action: CommitType::Standalone(vec![
                        committed_account,
                    ]),
                    undelegate_action: UndelegateType::Standalone,
                }),
                ..Default::default()
            }
        } else {
            MagicIntentBundle {
                commit: Some(CommitType::Standalone(vec![committed_account])),
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
            &validator,
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
    async fn eata_commit_without_materialization_metadata_requires_base_metadata(
    ) {
        let fetcher = Arc::new(EmptyFetcher);
        let (intent, validator) =
            eata_intent_without_materialization_metadata(false);

        let err = TaskBuilderImpl::commit_tasks(
            &fetcher,
            &intent,
            &validator,
            &None::<crate::persist::IntentPersisterImpl>,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            TaskBuilderError::CommitTasksBuildError(
                TaskInfoFetcherError::AccountNotFoundError(_)
            )
        ));
    }

    #[tokio::test]
    async fn rent_pending_undelegation_prepends_validator_specific_eata_delegate(
    ) {
        let fetcher = Arc::new(EmptyFetcher);
        let (intent, validator) = rent_pending_intent(true);

        let tasks = TaskBuilderImpl::commit_tasks(
            &fetcher,
            &intent,
            &validator,
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

        let tasks =
            TaskBuilderImpl::finalize_tasks(&fetcher, &intent, &validator)
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
    async fn rent_pending_commit_and_undelegate_uses_existing_reimbursement() {
        let fetcher = Arc::new(ExistingMetadataFetcher { next_nonce: 2 });
        let (intent, validator) = rent_pending_intent(true);

        let tasks =
            TaskBuilderImpl::finalize_tasks(&fetcher, &intent, &validator)
                .await
                .unwrap();

        assert!(matches!(
            tasks.as_slice(),
            [BaseTaskImpl::Finalize(_), BaseTaskImpl::Undelegate(_)]
        ));
        let BaseTaskImpl::Undelegate(undelegate) = &tasks[1] else {
            panic!("expected undelegate task");
        };
        assert_ne!(undelegate.rent_reimbursement, validator);
        assert_eq!(
            undelegate.rent_reimbursement,
            existing_metadata_rent_reimbursement()
        );
    }

    #[tokio::test]
    async fn eata_undelegation_without_materialization_metadata_requires_base_reimbursement(
    ) {
        let fetcher = Arc::new(EmptyFetcher);
        let (intent, validator) =
            eata_intent_without_materialization_metadata(true);

        let err =
            TaskBuilderImpl::finalize_tasks(&fetcher, &intent, &validator)
                .await
                .unwrap_err();

        assert!(matches!(
            err,
            TaskBuilderError::FinalizedTasksBuildError(
                TaskInfoFetcherError::AccountNotFoundError(_)
            )
        ));
    }

    #[tokio::test]
    async fn rent_pending_second_commit_uses_fetched_nonce() {
        let fetcher = Arc::new(ExistingMetadataFetcher { next_nonce: 2 });
        let (intent, validator) = rent_pending_intent(false);

        let tasks = TaskBuilderImpl::commit_tasks(
            &fetcher,
            &intent,
            &validator,
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
    async fn eata_commit_existing_metadata_uses_normal_commit() {
        let fetcher = Arc::new(ExistingMetadataFetcher { next_nonce: 2 });
        let (intent, validator) =
            eata_intent_without_materialization_metadata(false);

        let tasks = TaskBuilderImpl::commit_tasks(
            &fetcher,
            &intent,
            &validator,
            &None::<crate::persist::IntentPersisterImpl>,
        )
        .await
        .unwrap();

        assert!(matches!(tasks.as_slice(), [BaseTaskImpl::Commit(_)]));
        let BaseTaskImpl::Commit(commit) = &tasks[0] else {
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
                &validator,
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
