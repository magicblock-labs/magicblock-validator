use magicblock_core::intent::CommittedAccount;
use magicblock_program::magic_scheduled_base_intent::{
    CommitType, MagicIntentBundle, UndelegateType,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::{
    tasks::{
        commit_task::CommitDelivery,
        task_strategist::TaskStrategist,
        utils::{
            create_action_tasks, create_commit_finalize_task,
            create_commit_task, TransactionUtils,
        },
        BaseTask, BaseTaskImpl, FinalizeTask, UndelegateTask,
    },
    transactions::{serialized_transaction_size, MAX_TRANSACTION_WIRE_SIZE},
};

/// Checks whether an intent could ever fit on the base layer, so intents
/// that can never succeed can be refused up front instead of failing later
/// during execution
///
/// The check assumes the smallest possible task representation (buffer-mode
/// commits for oversized accounts, full address-lookup-table coverage,
/// 2-stage execution) -- if it doesn't fit even then, no amount of
/// optimization at execution time will make it fit. Unlike
/// [`TaskBuilderImpl`], this never fetches anything (no commit ids, no
/// base-layer state): it only needs to know whether a fit is *possible*,
/// not build the real tasks that will actually be executed.
/// NOTE: this assumes ALTs are used optimally
/// where number of ALTs is - pubkeys.div_ceil(256). It is impossible to know in advance
/// how many ALTs will be used.
pub struct IntentSizeValidator;

impl IntentSizeValidator {
    /// Returns `true` if `intent`'s commit and finalize transactions could
    /// both fit within [`MAX_TRANSACTION_WIRE_SIZE`]. If this returns
    /// `false`, the intent can never succeed and should be refused.
    pub fn fits(intent: &MagicIntentBundle) -> bool {
        Self::tasks_fit(&Self::commit_tasks(intent))
            && Self::tasks_fit(&Self::finalize_tasks(intent))
    }

    /// Builds the commit-stage tasks used for the size estimate: real
    /// standalone/base actions, and the smallest-by-default
    /// `Commit`/`CommitFinalize` task for each committed account.
    fn commit_tasks(intent: &MagicIntentBundle) -> Vec<BaseTaskImpl> {
        let mut tasks: Vec<BaseTaskImpl> =
            create_action_tasks(&intent.standalone_actions).collect();

        if let Some(ref commit) = intent.commit {
            tasks.extend(Self::commit_type_tasks(commit));
        }
        if let Some(ref cau) = intent.commit_and_undelegate {
            tasks.extend(Self::commit_type_tasks(&cau.commit_action));
        }
        if let Some(ref commit_finalize) = intent.commit_finalize {
            tasks.extend(Self::commit_finalize_type_tasks(commit_finalize));
        }
        if let Some(ref cfau) = intent.commit_finalize_and_undelegate {
            tasks.extend(Self::commit_finalize_type_tasks(&cfau.commit_action));
        }

        tasks
    }

    /// Builds the finalize-stage tasks used for the size estimate, mirroring
    /// [`crate::tasks::task_builder::TasksBuilder::finalize_tasks`] but
    /// without fetching rent reimbursements (a placeholder pubkey is used
    /// instead, since it doesn't affect instruction size).
    fn finalize_tasks(intent: &MagicIntentBundle) -> Vec<BaseTaskImpl> {
        fn finalize_task(account: &CommittedAccount) -> BaseTaskImpl {
            FinalizeTask {
                delegated_account: account.pubkey,
            }
            .into()
        }

        fn undelegate_task(account: &CommittedAccount) -> BaseTaskImpl {
            UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                // Real reimbursement pubkey is unknown here; doesn't affect size.
                rent_reimbursement: Pubkey::default(),
            }
            .into()
        }

        fn commit_type_finalize_tasks(
            commit_type: &CommitType,
        ) -> Vec<BaseTaskImpl> {
            let mut tasks: Vec<BaseTaskImpl> = commit_type
                .get_committed_accounts()
                .iter()
                .map(finalize_task)
                .collect();
            if let CommitType::WithBaseActions { base_actions, .. } =
                commit_type
            {
                tasks.extend(create_action_tasks(base_actions));
            }
            tasks
        }

        let mut tasks = Vec::new();

        if let Some(ref commit) = intent.commit {
            tasks.extend(commit_type_finalize_tasks(commit));
        }

        if let Some(ref cau) = intent.commit_and_undelegate {
            tasks.extend(commit_type_finalize_tasks(&cau.commit_action));
            tasks.extend(
                cau.commit_action
                    .get_committed_accounts()
                    .iter()
                    .map(undelegate_task),
            );
            if let UndelegateType::WithBaseActions(actions) =
                &cau.undelegate_action
            {
                tasks.extend(create_action_tasks(actions));
            }
        }

        // `commit_finalize` needs no separate finalize step: commit and
        // finalize already happen together in a single `CommitFinalizeTask`.
        if let Some(ref cfau) = intent.commit_finalize_and_undelegate {
            tasks.extend(
                cfau.commit_action
                    .get_committed_accounts()
                    .iter()
                    .map(undelegate_task),
            );
            if let UndelegateType::WithBaseActions(actions) =
                &cfau.undelegate_action
            {
                tasks.extend(create_action_tasks(actions));
            }
        }

        tasks
    }

    /// Builds the smallest-by-default `CommitTask` for `account`. Reuses
    /// [`create_commit_task`]'s real `COMMIT_STATE_SIZE_THRESHOLD` check by
    /// passing a clone of the account's own data as a stand-in base account
    /// -- large enough accounts land on `DiffInArgs`, which is then
    /// immediately escalated to buffer mode since the real diff size is
    /// unknowable ahead of time and a buffer instruction only ever
    /// references the buffer PDA, never account data.
    ///
    /// `allow_undelegation` is always a placeholder: it's a fixed-size flag
    /// in the instruction args and never changes instruction size, so which
    /// value we pass here doesn't matter. What actually differs between a
    /// commit and a commit-and-undelegate is the extra `UndelegateTask`
    /// built in [`Self::finalize_tasks`].
    fn commit_task(account: &CommittedAccount) -> BaseTaskImpl {
        let mut task = create_commit_task(
            0,
            false,
            account.clone(),
            Some(account.account.clone()),
        );
        if matches!(task.delivery_details, CommitDelivery::DiffInArgs { .. }) {
            task.try_optimize_tx_size();
        }
        task.into()
    }

    /// Same as [`Self::commit_task`] but for `CommitFinalizeTask`.
    fn commit_finalize_task(account: &CommittedAccount) -> BaseTaskImpl {
        let mut task = create_commit_finalize_task(
            0,
            false,
            account.clone(),
            Some(account.account.clone()),
        );
        if matches!(task.delivery, CommitDelivery::DiffInArgs { .. }) {
            task.try_optimize_tx_size();
        }
        task.into()
    }

    fn commit_type_tasks(commit_type: &CommitType) -> Vec<BaseTaskImpl> {
        let mut tasks: Vec<BaseTaskImpl> = commit_type
            .get_committed_accounts()
            .iter()
            .map(Self::commit_task)
            .collect();
        if let CommitType::WithBaseActions { base_actions, .. } = commit_type {
            tasks.extend(create_action_tasks(base_actions));
        }
        tasks
    }

    fn commit_finalize_type_tasks(
        commit_type: &CommitType,
    ) -> Vec<BaseTaskImpl> {
        let mut tasks: Vec<BaseTaskImpl> = commit_type
            .get_committed_accounts()
            .iter()
            .map(Self::commit_finalize_task)
            .collect();
        if let CommitType::WithBaseActions { base_actions, .. } = commit_type {
            tasks.extend(create_action_tasks(base_actions));
        }
        tasks
    }

    /// Returns `true` if `tasks`, assembled into a single transaction with
    /// full address-lookup-table coverage, fits within
    /// [`MAX_TRANSACTION_WIRE_SIZE`].
    fn tasks_fit(tasks: &[BaseTaskImpl]) -> bool {
        let placeholder = Keypair::new();
        let lookup_table_keys = TaskStrategist::collect_lookup_table_keys(
            &placeholder.pubkey(),
            tasks,
        );
        let lookup_tables =
            TransactionUtils::dummy_lookup_table(&lookup_table_keys);

        TransactionUtils::assemble_tasks_tx(
            &placeholder,
            tasks,
            0,
            &lookup_tables,
        )
        .map(|tx| serialized_transaction_size(&tx) <= MAX_TRANSACTION_WIRE_SIZE)
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use magicblock_program::magic_scheduled_base_intent::{
        BaseAction, ProgramArgs,
    };
    use solana_account::Account;

    use super::*;

    fn make_committed_account(data_len: usize) -> CommittedAccount {
        CommittedAccount {
            pubkey: Pubkey::new_unique(),
            account: Account {
                lamports: 1_000,
                data: vec![0; data_len],
                owner: Pubkey::new_unique(),
                executable: false,
                rent_epoch: 0,
            },
            remote_slot: 0,
        }
    }

    fn make_base_action(data_len: usize) -> BaseAction {
        BaseAction {
            compute_units: 10_000,
            destination_program: Pubkey::new_unique(),
            source_program: None,
            escrow_authority: Pubkey::new_unique(),
            data_per_program: ProgramArgs {
                escrow_index: 0,
                data: vec![0; data_len],
            },
            account_metas_per_program: vec![],
            callback: None,
        }
    }

    #[test]
    fn test_empty_intent_fits() {
        assert!(IntentSizeValidator::fits(&MagicIntentBundle::default()));
    }

    #[test]
    fn test_small_commit_fits() {
        let intent = MagicIntentBundle {
            commit: Some(CommitType::Standalone(vec![make_committed_account(
                10,
            )])),
            ..Default::default()
        };
        assert!(IntentSizeValidator::fits(&intent));
    }

    #[test]
    fn test_large_commit_forced_to_buffer_fits() {
        // Well above COMMIT_STATE_SIZE_THRESHOLD -- would never fit inline,
        // but must be escalated to buffer mode by the validator.
        let intent = MagicIntentBundle {
            commit: Some(CommitType::Standalone(vec![make_committed_account(
                50_000,
            )])),
            ..Default::default()
        };
        assert!(IntentSizeValidator::fits(&intent));
    }

    #[test]
    fn test_oversized_standalone_action_does_not_fit() {
        // BaseAction data is used as-is (never optimized away), so a
        // payload bigger than the whole transaction wire size can never fit.
        let intent = MagicIntentBundle {
            standalone_actions: vec![make_base_action(2_000)],
            ..Default::default()
        };
        assert!(!IntentSizeValidator::fits(&intent));
    }
}
