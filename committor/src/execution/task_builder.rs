use std::{collections::HashMap, sync::Arc};

use delegation_program_interface::UndelegationRequester;
use mbv_core::intent::{CommitAndUndelegate, CommitType, UndelegateType, types::CommittedAccount};
use program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::error;

use crate::shared::{
    account_state::{DelegationInfo, TaskInfoProvider, TaskInfoProviderError},
    tasks::{
        Task, UndelegateTask,
        utils::{COMMIT_STATE_SIZE_THRESHOLD, create_action_tasks, create_commit_finalize_task},
    },
};

/// Nonces and base account snapshots needed to build commit tasks.
struct CommitInputs {
    /// commit nonce for a given address
    commit_nonces: HashMap<Pubkey, u64>,
    /// Base account state for diff calculation
    base_accounts: HashMap<Pubkey, Account>,
    delegation_info: HashMap<Pubkey, DelegationInfo>,
}

/// Task builder
pub(crate) struct TaskBuilder;

impl TaskBuilder {
    fn get_follow_up_info_accounts(intent_bundle: &ScheduledIntentBundle) -> Vec<(Pubkey, u64)> {
        [
            intent_bundle.get_undelegate_intent_accounts(),
            intent_bundle.intent_bundle.get_commit_finalize_and_undelegate_intent_accounts(),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .map(|account| (account.pubkey, account.remote_slot))
        .collect()
    }

    async fn fetch_commit_inputs<C: TaskInfoProvider>(
        intent_bundle: &ScheduledIntentBundle,
        task_info_provider: &Arc<C>,
    ) -> TaskBuilderResult<CommitInputs> {
        // Fetch necessary data for commit task creation.
        let all_committed_accounts = intent_bundle.get_all_committed_accounts();
        // Get commit nonces and base accounts
        let min_context_slot = all_committed_accounts
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let committed_accounts = all_committed_accounts
            .iter()
            .map(|account| (account.pubkey, account.remote_slot))
            .collect::<Vec<_>>();
        let diffable_pubkeys = all_committed_accounts
            .iter()
            .filter(|account| account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD)
            .map(|account| account.pubkey)
            .collect::<Vec<_>>();
        let (commit_ids, base_accounts, delegation_info) = tokio::join!(
            task_info_provider.fetch_next_commit_nonces(&committed_accounts, min_context_slot),
            task_info_provider.get_base_accounts(&diffable_pubkeys, min_context_slot),
            task_info_provider.fetch_delegation_info(&committed_accounts, min_context_slot)
        );
        let commit_nonces = commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;
        let base_accounts = base_accounts.unwrap_or_else(|err| {
            tracing::warn!(intent_id = intent_bundle.intent_id, error = ?err, "Failed to fetch base accounts, falling back to full-state CommitFinalize");
            Default::default()
        });

        Ok(CommitInputs {
            commit_nonces,
            base_accounts,
            delegation_info: delegation_info.map_err(TaskBuilderError::CommitTasksBuildError)?,
        })
    }

    /// Builds commits and actions that can run before the follow-up transaction.
    pub(crate) async fn commit_tasks<C: TaskInfoProvider>(
        task_info_provider: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
    ) -> TaskBuilderResult<Vec<Task>> {
        let mut tasks = Vec::new();
        // Add standalone actions first
        tasks.extend(create_action_tasks(
            intent_bundle.standalone_actions().as_slice(),
        ));

        // Fetch data necessary for task creation
        let CommitInputs {
            mut commit_nonces,
            mut base_accounts,
            delegation_info,
        } = Self::fetch_commit_inputs(intent_bundle, task_info_provider).await?;

        // Create tasks per intent type
        if let Some(ref value) = intent_bundle.intent_bundle.commit {
            tasks.extend(build_commit_tasks(
                value,
                false,
                &mut commit_nonces,
                &mut base_accounts,
                &delegation_info,
            )?);
        }
        if let Some(ref value) = intent_bundle.intent_bundle.commit_finalize {
            tasks.extend(build_commit_tasks(
                value,
                false,
                &mut commit_nonces,
                &mut base_accounts,
                &delegation_info,
            )?);
            if let CommitType::WithBaseActions { base_actions, .. } = value {
                tasks.extend(create_action_tasks(base_actions));
            }
        }
        if let Some(ref value) = intent_bundle.intent_bundle.commit_and_undelegate {
            tasks.extend(build_commit_tasks(
                &value.commit_action,
                true,
                &mut commit_nonces,
                &mut base_accounts,
                &delegation_info,
            )?);
        }
        if let Some(ref value) = intent_bundle.intent_bundle.commit_finalize_and_undelegate {
            tasks.extend(build_commit_tasks(
                &value.commit_action,
                true,
                &mut commit_nonces,
                &mut base_accounts,
                &delegation_info,
            )?);
            if let CommitType::WithBaseActions { base_actions, .. } = &value.commit_action {
                tasks.extend(create_action_tasks(base_actions));
            }
        }

        Ok(tasks)
    }

    /// Returns tasks for remaining actions and undelegation
    pub(crate) async fn follow_up_tasks<C: TaskInfoProvider>(
        info_provider: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
    ) -> TaskBuilderResult<Vec<Task>> {
        // Helper to create an undelegate task
        fn undelegate_task(account: &CommittedAccount, info: &DelegationInfo) -> Task {
            UndelegateTask {
                version: info.version,
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: info.rent_payer,
                include_undelegation_request: info.undelegation_requester
                    == UndelegationRequester::OwnerProgram,
            }
            .into()
        }

        // Helper to process commit types
        fn create_follow_up_tasks(commit: &CommitType) -> Vec<Task> {
            match commit {
                CommitType::Standalone(_) => Vec::new(),
                CommitType::WithBaseActions { base_actions, .. } => {
                    // State is finalized in the commit stage. Keep ordinary
                    // commit actions here, before undelegation, as before.
                    create_action_tasks(base_actions).collect()
                }
            }
        }

        fn create_undelegate_tasks(
            commit_and_undelegate: &CommitAndUndelegate,
            delegation_info: &HashMap<Pubkey, DelegationInfo>,
        ) -> TaskBuilderResult<Vec<Task>> {
            let accounts = commit_and_undelegate.get_committed_accounts();

            let mut tasks = accounts
                .iter()
                .map(|account| {
                    let info = delegation_info_for_pubkey(account.pubkey, delegation_info)?;
                    Ok(undelegate_task(account, info))
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
        let follow_up_info_accounts = Self::get_follow_up_info_accounts(intent_bundle);
        let min_context_slot = intent_bundle
            .get_all_committed_accounts()
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let delegation_info = info_provider
            .fetch_delegation_info(&follow_up_info_accounts, min_context_slot)
            .await
            .map_err(TaskBuilderError::FollowUpTasksBuildError)?;

        if let Some(ref value) = intent_bundle.intent_bundle.commit {
            tasks.extend(create_follow_up_tasks(value));
        }

        if let Some(ref value) = intent_bundle.intent_bundle.commit_and_undelegate {
            tasks.extend(create_follow_up_tasks(&value.commit_action));
            tasks.extend(create_undelegate_tasks(value, &delegation_info)?);
        }

        if let Some(ref value) = intent_bundle.intent_bundle.commit_finalize_and_undelegate {
            tasks.extend(create_undelegate_tasks(value, &delegation_info)?);
        }

        Ok(tasks)
    }
}

fn build_commit_tasks(
    commit_type: &CommitType,
    allow_undelegation: bool,
    commit_nonces: &mut HashMap<Pubkey, u64>,
    base_accounts: &mut HashMap<Pubkey, Account>,
    delegation_info: &HashMap<Pubkey, DelegationInfo>,
) -> TaskBuilderResult<Vec<Task>> {
    commit_type
        .get_committed_accounts()
        .iter()
        .map(|account| {
            let nonce = take_commit_nonce(commit_nonces, account.pubkey);
            let base = base_accounts.remove(&account.pubkey);
            let info = delegation_info_for_pubkey(account.pubkey, delegation_info)?;
            let mut task =
                create_commit_finalize_task(nonce, allow_undelegation, account.clone(), base);
            task.version = info.version;
            Ok(task.into())
        })
        .collect()
}

fn take_commit_nonce(commit_nonces: &mut HashMap<Pubkey, u64>, pubkey: Pubkey) -> u64 {
    commit_nonces.remove(&pubkey).unwrap_or_else(|| {
        // This shall not ever happen since TaskInfoProvider
        // returns commit ids for all pubkeys or throws
        // If it does occur, it will be patched and retried by IntentExecutor
        error!(pubkey = %pubkey, "Commit id absent for pubkey");
        0
    })
}

fn delegation_info_for_pubkey(
    pubkey: Pubkey,
    delegation_info: &HashMap<Pubkey, DelegationInfo>,
) -> TaskBuilderResult<&DelegationInfo> {
    delegation_info
        .get(&pubkey)
        .ok_or(TaskBuilderError::MissingDelegationInfo(pubkey))
}

#[derive(thiserror::Error, Debug)]
pub enum TaskBuilderError {
    #[error("CommitIdFetchError: {0}")]
    CommitTasksBuildError(#[source] TaskInfoProviderError),
    #[error("FollowUpTasksBuildError: {0}")]
    FollowUpTasksBuildError(#[source] TaskInfoProviderError),
    #[error("Missing delegation info for {0}")]
    MissingDelegationInfo(Pubkey),
}

impl TaskBuilderError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::CommitTasksBuildError(err) => err.signature(),
            Self::FollowUpTasksBuildError(err) => err.signature(),
            Self::MissingDelegationInfo(_) => None,
        }
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Self::CommitTasksBuildError(err) | Self::FollowUpTasksBuildError(err) => {
                err.is_transient()
            }
            Self::MissingDelegationInfo(_) => false,
        }
    }
}

pub(crate) type TaskBuilderResult<T, E = TaskBuilderError> = Result<T, E>;
