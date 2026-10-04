use std::collections::HashSet;

use magicblock_core::intent::{
    types::CommittedAccount, CommitType, MagicIntentBundle, UndelegateType,
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
    transactions::{
        serialized_transaction_size, MAX_TRANSACTION_V1_WIRE_SIZE,
        MAX_TRANSACTION_WIRE_SIZE,
    },
};

/// Estimates whether an intent's commit and finalize stages fit the supported
/// transaction formats without fetching base-layer state or commit metadata.
///
/// Unlike [`crate::tasks::task_builder::TasksBuilder`], admission cannot compute
/// the actual account diff. It estimates larger accounts using buffers, reserves
/// distinct keys for unknown rent payers, and includes a uniqueness noop in each
/// stage. Each stage is checked against v1, then v0 with full ALT coverage.
/// Actual ALT coverage is unknown at admission time.
///
/// This estimate can reject an intent that a smaller execution-time diff would
/// allow. Passing the estimate also does not guarantee execution succeeds:
/// preparation and base-layer execution still enforce their own requirements.
pub struct IntentSizeValidator;

impl IntentSizeValidator {
    /// Returns whether both estimated stages fit v1 or the v0 + ALT fallback.
    /// A rejection applies to these estimates, not every possible delivery plan.
    pub fn fits(intent: &MagicIntentBundle) -> bool {
        Self::commit_fits(intent) && Self::finalize_fits(intent)
    }

    /// Checks the commit-stage tasks fit. Always assumes a uniqueness noop
    /// is present - worst case, since we can't rule it out ahead of time.
    fn commit_fits(intent: &MagicIntentBundle) -> bool {
        Self::tasks_fit(Self::commit_tasks(intent), Some(0))
    }

    /// Checks the finalize-stage tasks fit; same worst-case noop assumption.
    fn finalize_fits(intent: &MagicIntentBundle) -> bool {
        Self::tasks_fit(Self::finalize_tasks(intent), Some(0))
    }

    /// Builds the commit-stage tasks used for the size estimate: real
    /// standalone/base actions, and an estimated `Commit`/`CommitFinalize`
    /// task for each committed account.
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
    /// without fetching rent payers. [`Self::tasks_fit`] assigns distinct
    /// placeholder keys before compiling these tasks, accounting for each
    /// unknown payer's contribution to the transaction size and account count.
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
                // Assigned a distinct placeholder before the fit checks.
                rent_reimbursement: Pubkey::default(),
                // We lack context here so let's assume worst case scenario
                include_undelegation_request: true,
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

    /// Builds the estimated `CommitTask` for `account`. Reuses
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

    /// Builds the commit-stage tasks for `commit_type`'s accounts.
    /// `WithBaseActions` actions are excluded: they run in the finalize
    /// stage, not the commit stage.
    fn commit_type_tasks(commit_type: &CommitType) -> Vec<BaseTaskImpl> {
        commit_type
            .get_committed_accounts()
            .iter()
            .map(Self::commit_task)
            .collect()
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

    /// Returns `true` if `tasks` plus `uniqueness_nonce` (if any) fit through
    /// either v1 or the v0 + ALT fallback.
    fn tasks_fit(
        mut tasks: Vec<BaseTaskImpl>,
        uniqueness_nonce: Option<u64>,
    ) -> bool {
        if TransactionUtils::tasks_compute_units(&tasks) > 1_400_000 {
            return false;
        }

        let placeholder = Keypair::new();
        Self::assign_unknown_rent_payers(
            &mut tasks,
            &placeholder.pubkey(),
            uniqueness_nonce,
        );
        Self::tasks_fit_v1(&placeholder, &tasks, uniqueness_nonce)
            || Self::tasks_fit_v0_with_alts(
                &placeholder,
                &tasks,
                uniqueness_nonce,
            )
    }

    /// Give unknown payers separate keys so account-key deduplication cannot
    /// make the estimate smaller. Only admission tasks use these placeholders;
    /// execution gets the actual payers from delegation metadata.
    fn assign_unknown_rent_payers(
        tasks: &mut [BaseTaskImpl],
        authority: &Pubkey,
        uniqueness_nonce: Option<u64>,
    ) {
        if !tasks
            .iter()
            .any(|task| matches!(task, BaseTaskImpl::Undelegate(_)))
        {
            return;
        }

        let mut used_keys = HashSet::from([*authority]);
        for task in tasks.iter() {
            let instruction = task.instruction(authority);
            used_keys.insert(instruction.program_id);
            used_keys
                .extend(instruction.accounts.iter().map(|meta| meta.pubkey));
        }
        for instruction in TransactionUtils::budget_instructions(0, 0, 0) {
            used_keys.insert(instruction.program_id);
        }
        if let Some(nonce) = uniqueness_nonce {
            used_keys.insert(
                TransactionUtils::uniqueness_noop_instruction(nonce).program_id,
            );
        }

        // Counter-derived keys keep the estimate reproducible. Each occupied
        // candidate is skipped once, so the work is bounded by the stage's keys.
        let mut counter = 0_u64;
        for task in tasks {
            if let BaseTaskImpl::Undelegate(task) = task {
                loop {
                    let mut bytes = [0; 32];
                    bytes[..8].copy_from_slice(&counter.to_le_bytes());
                    counter += 1;
                    let payer = Pubkey::new_from_array(bytes);
                    if used_keys.insert(payer) {
                        task.rent_reimbursement = payer;
                        break;
                    }
                }
            }
        }
    }

    fn tasks_fit_v1(
        placeholder: &Keypair,
        tasks: &[BaseTaskImpl],
        uniqueness_nonce: Option<u64>,
    ) -> bool {
        TransactionUtils::assemble_tasks_v1_tx_with_uniqueness_nonce(
            placeholder,
            tasks,
            0,
            uniqueness_nonce,
        )
        .map(|tx| tx.serialized_size() <= MAX_TRANSACTION_V1_WIRE_SIZE)
        .unwrap_or(false)
    }

    fn tasks_fit_v0_with_alts(
        placeholder: &Keypair,
        tasks: &[BaseTaskImpl],
        uniqueness_nonce: Option<u64>,
    ) -> bool {
        let lookup_table_keys = TaskStrategist::collect_lookup_table_keys(
            &placeholder.pubkey(),
            tasks,
            uniqueness_nonce,
        );
        let lookup_tables =
            TransactionUtils::dummy_lookup_table(&lookup_table_keys);

        TransactionUtils::assemble_tasks_tx_with_uniqueness_nonce(
            placeholder,
            tasks,
            0,
            &lookup_tables,
            uniqueness_nonce,
        )
        .map(|tx| serialized_transaction_size(&tx) <= MAX_TRANSACTION_WIRE_SIZE)
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use magicblock_core::intent::{
        BaseAction, CommitAndUndelegate, ProgramArgs,
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
            id: 0,
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
    fn test_undelegation_admission_reserves_unknown_rent_payer_keys() {
        let authority = Keypair::new();
        let counter_key = |counter: u64| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&counter.to_le_bytes());
            Pubkey::new_from_array(bytes)
        };
        let mut accounts =
            vec![make_committed_account(10), make_committed_account(10)];
        let mut action = make_base_action(0);
        // Occupy the first candidates with real instruction keys. A placeholder
        // must not alias the destination, escrow, owner, or delegated account.
        action.destination_program = counter_key(1);
        action.escrow_authority = counter_key(2);
        accounts[0].account.owner = counter_key(3);
        accounts[1].account.owner = counter_key(3);
        accounts[0].pubkey = counter_key(4);

        let make_intent = |data_len, commit_finalize| {
            let mut action = action.clone();
            action.data_per_program.data.resize(data_len, 0);
            let commit = CommitAndUndelegate {
                commit_action: CommitType::Standalone(accounts.clone()),
                undelegate_action: UndelegateType::WithBaseActions(vec![
                    action,
                ]),
            };
            if commit_finalize {
                MagicIntentBundle {
                    commit_finalize_and_undelegate: Some(commit),
                    ..Default::default()
                }
            } else {
                MagicIntentBundle {
                    commit_and_undelegate: Some(commit),
                    ..Default::default()
                }
            }
        };
        let actual_payers = [Pubkey::new_unique(), Pubkey::new_unique()];
        let actual_tasks = |intent: &MagicIntentBundle| {
            let mut tasks = IntentSizeValidator::finalize_tasks(intent);
            let mut payers = actual_payers.iter();
            for task in &mut tasks {
                if let BaseTaskImpl::Undelegate(task) = task {
                    task.rent_reimbursement = *payers.next().unwrap();
                }
            }
            tasks
        };
        let wire_size = |tasks: &[BaseTaskImpl]| {
            TransactionUtils::assemble_tasks_v1_tx_with_uniqueness_nonce(
                &authority,
                tasks,
                0,
                Some(0),
            )
            .unwrap()
            .serialized_size()
        };
        let payer_keys = |tasks: &[BaseTaskImpl]| {
            tasks
                .iter()
                .filter_map(|task| match task {
                    BaseTaskImpl::Undelegate(task) => {
                        Some(task.rent_reimbursement)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        for commit_finalize in [false, true] {
            // Measure actual instructions with distinct metadata rent payers.
            // v1 uses fixed-size data-length fields, so adding this payload
            // reaches the exact wire boundary without hard-coding task sizes.
            let boundary_len = MAX_TRANSACTION_V1_WIRE_SIZE
                - wire_size(&actual_tasks(&make_intent(0, commit_finalize)));
            let at_boundary = make_intent(boundary_len, commit_finalize);
            assert_eq!(
                wire_size(&actual_tasks(&at_boundary)),
                MAX_TRANSACTION_V1_WIRE_SIZE
            );
            assert!(IntentSizeValidator::fits(&at_boundary));

            let above_boundary = make_intent(boundary_len + 1, commit_finalize);
            let actual = actual_tasks(&above_boundary);
            assert_eq!(wire_size(&actual), MAX_TRANSACTION_V1_WIRE_SIZE + 1);
            assert!(!IntentSizeValidator::tasks_fit_v0_with_alts(
                &authority,
                &actual,
                Some(0)
            ));
            assert!(!IntentSizeValidator::fits(&above_boundary));

            // Sharing the old default payer would incorrectly admit this case.
            let original = IntentSizeValidator::finalize_tasks(&above_boundary);
            assert!(IntentSizeValidator::tasks_fit_v1(
                &authority,
                &original,
                Some(0)
            ));
            let mut estimated = original.clone();
            IntentSizeValidator::assign_unknown_rent_payers(
                &mut estimated,
                &authority.pubkey(),
                Some(0),
            );
            let payers = payer_keys(&estimated);
            assert_eq!(payers.len(), 2);
            assert_ne!(payers[0], payers[1]);
            let mut known_keys = TaskStrategist::collect_lookup_table_keys(
                &authority.pubkey(),
                &original,
                Some(0),
            );
            known_keys.push(authority.pubkey());
            known_keys.extend(original.iter().map(BaseTask::program_id));
            assert!(payers.iter().all(|payer| !known_keys.contains(payer)));
            assert_eq!(wire_size(&estimated), wire_size(&actual));

            let mut repeated = original;
            IntentSizeValidator::assign_unknown_rent_payers(
                &mut repeated,
                &authority.pubkey(),
                Some(0),
            );
            assert_eq!(payer_keys(&repeated), payers);
            assert!(IntentSizeValidator::fits(&at_boundary));
            assert!(!IntentSizeValidator::fits(&above_boundary));
        }
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
        // The actual diff is unknown at admission time, so an account above
        // COMMIT_STATE_SIZE_THRESHOLD is estimated using a buffer.
        let intent = MagicIntentBundle {
            commit: Some(CommitType::Standalone(vec![make_committed_account(
                10_240,
            )])),
            ..Default::default()
        };
        assert!(IntentSizeValidator::fits(&intent));
    }

    #[test]
    fn test_v1_sized_standalone_action_fits() {
        let intent = MagicIntentBundle {
            standalone_actions: vec![make_base_action(2_000)],
            ..Default::default()
        };
        assert!(IntentSizeValidator::fits(&intent));
    }

    #[test]
    fn test_oversized_standalone_action_does_not_fit() {
        let intent = MagicIntentBundle {
            standalone_actions: vec![make_base_action(5_000)],
            ..Default::default()
        };
        assert!(!IntentSizeValidator::fits(&intent));
    }
}
