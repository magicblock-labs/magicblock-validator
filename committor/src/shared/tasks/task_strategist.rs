use std::collections::BinaryHeap;

use mbv_core::intent::BaseActionCallback;
use solana_keypair::Keypair;
use solana_message::CompileError;
use solana_pubkey::Pubkey;
use solana_signer::{Signer, SignerError};

use crate::shared::{
    tasks::{BaseActionTask, Task, utils::TransactionUtils},
    transaction::{
        MAX_TRANSACTION_V1_WIRE_SIZE, MAX_TRANSACTION_WIRE_SIZE, serialized_transaction_size,
    },
};

#[derive(Default, Clone)]
pub(crate) struct TransactionStrategy {
    pub tasks: Vec<Task>,
    pub lookup_tables_keys: Vec<Pubkey>,
    // TODO(edwin): remove this
    pub uniqueness_nonce: Option<u64>,
}

impl TransactionStrategy {
    pub(crate) fn task_instruction_offset(&self) -> u8 {
        if self.uses_alts() {
            TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT
        } else {
            0
        }
    }

    /// In case old strategy used ALTs recalculate old value
    /// NOTE: this can be used when full revaluation is unnecessary, like:
    /// some tasks were reset, number of tasks didn't increase
    pub(crate) fn refresh_lookup_table_keys(&mut self, authority: &Pubkey) {
        if self.uses_alts() {
            self.lookup_tables_keys = TaskStrategist::collect_lookup_table_keys(
                authority,
                &self.tasks,
                self.uniqueness_nonce,
            );
        }
    }

    /// Extracts callbacks from actions
    pub(crate) fn extract_action_callbacks(&mut self) -> Vec<BaseActionCallback> {
        self.tasks
            .iter_mut()
            .filter_map(
                |el| {
                    if let Task::BaseAction(value) = el { Some(value) } else { None }
                },
            )
            .filter_map(BaseActionTask::extract_callback)
            .collect()
    }

    /// Removes failed actions and returns their callbacks.
    pub(crate) fn remove_actions(&mut self, authority: &Pubkey) -> Vec<BaseActionCallback> {
        let callbacks = self.extract_action_callbacks();
        self.tasks.retain(|task| !matches!(task, Task::BaseAction(_)));
        self.refresh_lookup_table_keys(authority);
        callbacks
    }

    pub(crate) fn has_actions_callbacks(&self) -> bool {
        self.tasks
            .iter()
            .filter_map(
                |el| {
                    if let Task::BaseAction(value) = el { Some(value) } else { None }
                },
            )
            .any(BaseActionTask::has_callback)
    }

    pub(crate) fn uses_alts(&self) -> bool {
        !self.lookup_tables_keys.is_empty()
    }
}

pub(crate) struct TransactionPlan {
    pub current: TransactionStrategy,
    pub follow_up: Option<TransactionStrategy>,
}

impl TransactionPlan {
    pub(crate) fn uses_alts(&self) -> bool {
        self.current.uses_alts()
            || self.follow_up.as_ref().is_some_and(TransactionStrategy::uses_alts)
    }
}

impl From<TransactionStrategy> for TransactionPlan {
    fn from(current: TransactionStrategy) -> Self {
        Self { current, follow_up: None }
    }
}

/// Takes [`Task`]s and chooses the best way to fit them in TX
/// It may change Task execution strategy so all task would fit in tx
pub(crate) struct TaskStrategist;
impl TaskStrategist {
    /// Fits tasks into transactions and chooses a combined or split plan.
    pub(crate) fn build_plan(
        commit_tasks: Vec<Task>,
        follow_up_tasks: Vec<Task>,
        authority: &Pubkey,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<TransactionPlan> {
        const MAX_UNITED_TASKS_LEN: usize = 22;
        // Combined commits need no empty follow-up transaction.
        if follow_up_tasks.is_empty()
            && commit_tasks.iter().any(|task| matches!(task, Task::CommitFinalize(_)))
        {
            return Self::build_strategy(commit_tasks, authority, uniqueness_nonce).map(Into::into);
        }

        // We can unite in 1 tx a lot of commits
        // but then there's a possibility of hitting CPI limit, aka
        // MaxInstructionTraceLengthExceeded error.
        // So we limit tasks len with 22 total tasks
        // The runner can also split a combined transaction after a recoverable limit error.
        if commit_tasks.len() + follow_up_tasks.len() > MAX_UNITED_TASKS_LEN {
            return Self::build_split_plan(
                commit_tasks,
                follow_up_tasks,
                authority,
                uniqueness_nonce,
            );
        }

        // Clone tasks since strategies applied to united case maybe suboptimal for regular one
        // Unite tasks to attempt running as single tx
        let combined_tasks = [commit_tasks.clone(), follow_up_tasks.clone()].concat();
        let combined_plan =
            match TaskStrategist::build_strategy(combined_tasks, authority, uniqueness_nonce) {
                Ok(strategy) => TransactionPlan::from(strategy),
                Err(TaskStrategistError::FailedToFit) => {
                    // Split the tasks when they do not fit together.
                    return Self::build_split_plan(
                        commit_tasks,
                        follow_up_tasks,
                        authority,
                        uniqueness_nonce,
                    );
                }
                Err(TaskStrategistError::Signer(err)) => {
                    return Err(err.into());
                }
                Err(err @ TaskStrategistError::Compile(_)) => return Err(err),
            };

        // If ALTs aren't used then we sure this will be optimal - return
        if !combined_plan.uses_alts() {
            return Ok(combined_plan);
        }

        // Prefer splitting when it avoids waiting for ALT activation.
        let split_plan =
            Self::build_split_plan(commit_tasks, follow_up_tasks, authority, uniqueness_nonce)?;
        if split_plan.uses_alts() { Ok(combined_plan) } else { Ok(split_plan) }
    }

    pub(crate) fn build_split_plan(
        commit_tasks: Vec<Task>,
        follow_up_tasks: Vec<Task>,
        authority: &Pubkey,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<TransactionPlan> {
        // Build the commit transaction.
        let commit_strategy =
            TaskStrategist::build_strategy(commit_tasks, authority, uniqueness_nonce)?;

        // Build its follow-up transaction.
        let follow_up_strategy =
            TaskStrategist::build_strategy(follow_up_tasks, authority, uniqueness_nonce)?;

        Ok(TransactionPlan {
            current: commit_strategy,
            follow_up: Some(follow_up_strategy),
        })
    }

    /// Returns [`TransactionStrategy`] for tasks
    /// Returns Error if all optimizations weren't enough
    pub(crate) fn build_strategy(
        tasks: Vec<Task>,
        validator: &Pubkey,
        uniqueness_nonce: Option<u64>,
    ) -> TaskStrategistResult<TransactionStrategy> {
        if TransactionUtils::tasks_compute_units(&tasks, uniqueness_nonce) > 1_400_000 {
            return Err(TaskStrategistError::FailedToFit);
        }
        let mut optimized_tasks = tasks.clone();
        if Self::try_optimize_tx_size_if_needed(&mut optimized_tasks, uniqueness_nonce)?
            <= MAX_TRANSACTION_V1_WIRE_SIZE
        {
            return Ok(TransactionStrategy {
                tasks: optimized_tasks,
                lookup_tables_keys: vec![],
                uniqueness_nonce,
            });
        }
        let optimized_tasks = if Self::attempt_lookup_tables(&tasks, uniqueness_nonce) {
            tasks
        } else if Self::attempt_lookup_tables(&optimized_tasks, uniqueness_nonce) {
            optimized_tasks
        } else {
            return Err(TaskStrategistError::FailedToFit);
        };
        let lookup_tables_keys =
            Self::collect_lookup_table_keys(validator, &optimized_tasks, uniqueness_nonce);
        Ok(TransactionStrategy {
            tasks: optimized_tasks,
            lookup_tables_keys,
            uniqueness_nonce,
        })
    }

    /// Attempt to use ALTs for ALL keys in tx
    /// Returns `true` if ALTs make tx fit, otherwise `false`
    /// TODO(edwin): optimize to use only necessary amount of pubkeys
    pub(crate) fn attempt_lookup_tables(tasks: &[Task], uniqueness_nonce: Option<u64>) -> bool {
        if TransactionUtils::tasks_compute_units(tasks, uniqueness_nonce) > 1_400_000 {
            return false;
        }
        let placeholder = Keypair::new();
        let dummy_lookup_tables = TransactionUtils::dummy_lookup_table(
            &Self::collect_lookup_table_keys(&placeholder.pubkey(), tasks, uniqueness_nonce),
        );

        // Assemble through the same path used for the real transaction so
        // fit decisions cannot diverge from the assembled message.
        match TransactionUtils::assemble_tasks_tx_with_uniqueness_nonce(
            &placeholder,
            tasks,
            u64::default(),
            &dummy_lookup_tables,
            uniqueness_nonce,
        ) {
            Ok(tx) => serialized_transaction_size(&tx) <= MAX_TRANSACTION_WIRE_SIZE,
            // Transaction doesn't fit, see CompileError
            Err(_) => false,
        }
    }

    pub(crate) fn collect_lookup_table_keys(
        authority: &Pubkey,
        tasks: &[Task],
        uniqueness_nonce: Option<u64>,
    ) -> Vec<Pubkey> {
        let budgets = TransactionUtils::tasks_compute_units(tasks, uniqueness_nonce);
        let size_budgets = TransactionUtils::tasks_accounts_size_budget(tasks);
        let mut service_instructions =
            TransactionUtils::budget_instructions(budgets, u64::default(), size_budgets).to_vec();
        if let Some(nonce) = uniqueness_nonce {
            service_instructions.push(TransactionUtils::uniqueness_noop_instruction(nonce));
        }

        TransactionUtils::unique_involved_pubkeys(tasks, authority, &service_instructions)
    }

    /// Optimizes tasks to fit the v1 wire limit.
    /// Returns Ok(size of tx after optimizations) else Err(SignerError).
    /// Note that the returned size, though possibly optimized one, may still not be under
    /// the v1 wire limit. The caller needs to check and make decision accordingly.
    fn try_optimize_tx_size_if_needed(
        tasks: &mut [Task],
        uniqueness_nonce: Option<u64>,
    ) -> Result<usize, SignerError> {
        // Get initial transaction size
        let calculate_tx_length = |tasks: &[Task]| {
            // Include the constant-size uniqueness noop so fit decisions
            // match the assembled transaction.
            match TransactionUtils::assemble_tasks_v1_tx_with_uniqueness_nonce(
                &Keypair::new(), // placeholder
                tasks,
                u64::default(), // placeholder
                uniqueness_nonce,
            ) {
                Ok(tx) => Ok(serialized_transaction_size(&tx)),
                Err(TaskStrategistError::FailedToFit) => Ok(usize::MAX),
                Err(TaskStrategistError::Compile(_)) => Ok(usize::MAX),
                Err(TaskStrategistError::Signer(err)) => Err(err),
            }
        };

        // Get initial transaction size
        let mut current_tx_length = calculate_tx_length(tasks)?;

        if current_tx_length <= MAX_TRANSACTION_V1_WIRE_SIZE {
            return Ok(current_tx_length);
        }

        // Create heap size -> index
        let ixs = TransactionUtils::tasks_instructions(&Pubkey::new_unique(), tasks);
        // Possible serialization failures are possible only due to size in our case
        // In that case we set size to max
        let sizes = ixs
            .iter()
            .map(|ix| wincode::serialized_size(ix).unwrap_or(u64::MAX))
            .map(|size| usize::try_from(size).unwrap_or(usize::MAX))
            .collect::<Vec<_>>();
        let mut map = sizes
            .into_iter()
            .enumerate()
            .map(|(index, size)| (size, index))
            .collect::<BinaryHeap<_>>();

        // We keep popping heaviest el-ts & try to optimize while heap is non-empty
        while let Some((_, index)) = map.pop() {
            if current_tx_length <= MAX_TRANSACTION_V1_WIRE_SIZE {
                break;
            }

            if tasks[index].try_use_buffer() {
                // Buffer delivery is the only conversion, so each task needs one attempt.
                current_tx_length = calculate_tx_length(tasks)?;
            }
        }

        Ok(current_tx_length)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TaskStrategistError {
    #[error("Failed to fit in single TX")]
    FailedToFit,
    #[error("Failed to compile transaction: {0}")]
    Compile(#[from] CompileError),
    #[error("SignerError: {0}")]
    Signer(#[from] SignerError),
}

pub(crate) type TaskStrategistResult<T, E = TaskStrategistError> = Result<T, E>;

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use dpi::legacy::{discriminator::DlpDiscriminator, state::UndelegationRequester};
    use mbv_core::intent::{
        BaseAction, CommitAndUndelegate, CommitType, MagicBaseIntent, ProgramArgs, UndelegateType,
        types::CommittedAccount,
    };
    use program::args::ShortAccountMeta;
    use solana_account::Account;
    use solana_pubkey::Pubkey;

    use super::*;
    use crate::{
        execution::{scheduler::create_test_intent_bundle, task_builder::TaskBuilder},
        shared::tasks::{
            BaseActionTask, BaseActionTaskV1, UndelegateTask,
            commit_delivery::CommitDelivery,
            commit_finalize_task::CommitFinalizeTask,
            utils::{COMMIT_STATE_SIZE_THRESHOLD, create_commit_finalize_task},
        },
        test_utils::{self, MockInfoProvider},
    };

    // Helper to create a simple commit task
    fn create_test_commit_task(
        commit_id: u64,
        data_size: usize,
        diff_len: usize,
    ) -> CommitFinalizeTask {
        let committed_account = CommittedAccount {
            pubkey: Pubkey::new_unique(),
            account: Account {
                lamports: 1000,
                data: vec![1; data_size],
                ..Default::default()
            },
            remote_slot: Default::default(),
        };

        if diff_len == 0 {
            create_commit_finalize_task(commit_id, false, committed_account, None)
        } else {
            let base_account = {
                let mut acc = committed_account.account.clone();
                assert!(diff_len <= acc.data.len());
                for byte in &mut acc.data[..diff_len] {
                    *byte = byte.wrapping_add(1);
                }
                acc
            };
            create_commit_finalize_task(commit_id, false, committed_account, Some(base_account))
        }
    }

    // Helper to create a Base action task
    fn create_test_base_action_task(len: usize) -> BaseActionTask {
        BaseActionTaskV1 {
            action: BaseAction {
                id: 0,
                destination_program: Pubkey::new_unique(),
                source_program: None,
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: vec![],
                data_per_program: ProgramArgs {
                    data: vec![0; len],
                    escrow_index: 0,
                },
                compute_units: 30_000,
                callback: None,
            },
        }
        .into()
    }

    fn commit_delivery(task: &Task) -> &CommitDelivery {
        let Task::CommitFinalize(task) = task else {
            panic!("expected a combined commit");
        };
        &task.delivery
    }

    // Helper to create an undelegate task
    fn create_test_undelegate_task() -> UndelegateTask {
        UndelegateTask {
            delegated_account: Pubkey::new_unique(),
            owner_program: Pubkey::default(),
            rent_reimbursement: Pubkey::new_unique(),
            include_undelegation_request: false,
        }
    }

    #[test]
    fn test_build_strategy_with_single_small_task() {
        test_utils::init_test_logger();
        let validator = Pubkey::new_unique();
        let task = create_test_commit_task(1, 100, 0);
        let tasks = vec![task.into()];

        let strategy =
            TaskStrategist::build_strategy(tasks, &validator, None).expect("Should build strategy");

        assert_eq!(strategy.tasks.len(), 1);
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_optimizes_to_buffer_when_needed() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 5000, 0); // Large task
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(tasks, &validator, None)
            .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.tasks.len(), 1);
        assert!(matches!(
            commit_delivery(&strategy.tasks[0]),
            CommitDelivery::StateInBuffer
        ));
    }

    #[test]
    fn test_build_strategy_buffers_payload_above_u16_limit() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 66_000, 0); // Large task
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(tasks, &validator, None)
            .expect("Buffer delivery must fit even when inline data exceeds u16");

        assert!(matches!(
            commit_delivery(&strategy.tasks[0]),
            CommitDelivery::StateInBuffer
        ));
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_does_not_optimize_large_account_but_small_diff() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 10_240, COMMIT_STATE_SIZE_THRESHOLD); // large account but small diff
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(tasks, &validator, None)
            .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.tasks.len(), 1);
        assert!(matches!(
            commit_delivery(&strategy.tasks[0]),
            CommitDelivery::DiffInArgs { .. }
        ));
    }

    #[test]
    fn test_build_strategy_does_not_optimize_large_account_and_above_threshold_diff() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 10_240, COMMIT_STATE_SIZE_THRESHOLD + 1); // large account but small diff
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(tasks, &validator, None)
            .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.tasks.len(), 1);
        assert!(matches!(
            commit_delivery(&strategy.tasks[0]),
            CommitDelivery::DiffInArgs { .. }
        ));
    }

    #[test]
    fn test_build_strategy_does_optimize_large_account_and_large_diff() {
        let validator = Pubkey::new_unique();

        let task = create_test_commit_task(1, 10_240, 5000);
        let tasks = vec![task.into()];

        let strategy = TaskStrategist::build_strategy(tasks, &validator, None)
            .expect("Should build strategy with buffer optimization");

        assert_eq!(strategy.tasks.len(), 1);
        assert!(matches!(
            commit_delivery(&strategy.tasks[0]),
            CommitDelivery::DiffInBuffer { .. }
        ));
    }

    #[test]
    fn test_build_strategy_creates_multiple_buffers() {
        // TODO: ALSO MAX NUM WITH PURE BUFFER commits, no alts
        const NUM_COMMITS: u64 = 3;

        let validator = Pubkey::new_unique();

        let tasks = (0..NUM_COMMITS)
            .map(|i| {
                let task = create_test_commit_task(i, 5000, 0); // Large task
                task.into()
            })
            .collect();

        let strategy = TaskStrategist::build_strategy(tasks, &validator, None)
            .expect("Should build strategy with buffer optimization");

        for optimized_task in &strategy.tasks {
            assert!(matches!(
                commit_delivery(optimized_task),
                CommitDelivery::StateInBuffer
            ));
        }
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_with_lookup_tables_when_needed() {
        // Eleven combined commits fit within the compute budget. Extra action
        // accounts push their buffered form beyond V1's static-key limit.
        const NUM_COMMITS: u64 = 11;

        let validator = Pubkey::new_unique();

        let mut tasks: Vec<Task> = (0..NUM_COMMITS)
            .map(|i| {
                // Large task
                let task = create_test_commit_task(i, 1000, 0);
                task.into()
            })
            .collect();
        let mut action = create_test_base_action_task(0);
        let BaseActionTask::V1(action_task) = &mut action else {
            panic!("expected a V1 action");
        };
        action_task.action.account_metas_per_program = (0..20)
            .map(|_| ShortAccountMeta {
                pubkey: Pubkey::new_unique(),
                is_writable: false,
            })
            .collect();
        tasks.push(action.into());

        let strategy = TaskStrategist::build_strategy(tasks, &validator, None)
            .expect("Should build strategy with buffer optimization");

        for optimized_task in &strategy.tasks[..NUM_COMMITS as usize] {
            assert!(matches!(
                commit_delivery(optimized_task),
                CommitDelivery::StateInBuffer
            ));
        }
        assert!(!strategy.lookup_tables_keys.is_empty());
    }

    #[test]
    fn test_build_strategy_reserves_space_for_uniqueness_nonce() {
        let validator = Pubkey::new_unique();
        let build = |data_len, nonce| {
            TaskStrategist::build_strategy(
                vec![create_test_base_action_task(data_len).into()],
                &validator,
                nonce,
            )
        };

        let no_nonce_boundary_len = (0..MAX_TRANSACTION_V1_WIRE_SIZE)
            .rev()
            .find(|data_len| {
                build(*data_len, None).is_ok()
                    && matches!(
                        build(*data_len, Some(42)),
                        Err(TaskStrategistError::FailedToFit)
                    )
            })
            .expect("expected a payload size at the v1 boundary");
        let nonce_boundary_len = (0..no_nonce_boundary_len)
            .rev()
            .find(|data_len| build(*data_len, Some(42)).is_ok())
            .expect("expected a nonce-bearing payload size to fit");

        let strategy =
            build(nonce_boundary_len, Some(42)).expect("should fit below the nonce boundary");
        assert_eq!(strategy.uniqueness_nonce, Some(42));
    }

    #[test]
    fn test_build_strategy_tries_v0_alts_before_buffering_v1_key_overflow() {
        let validator = Pubkey::new_unique();
        let action: BaseActionTask = BaseActionTaskV1 {
            action: BaseAction {
                id: 0,
                destination_program: Pubkey::new_unique(),
                source_program: None,
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: (0..65)
                    .map(|_| ShortAccountMeta {
                        pubkey: Pubkey::new_unique(),
                        is_writable: false,
                    })
                    .collect(),
                data_per_program: ProgramArgs { data: vec![], escrow_index: 0 },
                compute_units: 30_000,
                callback: None,
            },
        }
        .into();

        let strategy = TaskStrategist::build_strategy(vec![action.into()], &validator, None)
            .expect("should fall back to v0 + ALTs");

        assert!(!strategy.lookup_tables_keys.is_empty());
        assert!(matches!(strategy.tasks.as_slice(), [Task::BaseAction(_)]));
    }

    #[test]
    fn test_build_strategy_fails_when_cant_fit() {
        const NUM_COMMITS: u64 = 23;

        let validator = Pubkey::new_unique();

        let tasks = (0..NUM_COMMITS)
            .map(|i| {
                // Large task
                let task = create_test_commit_task(i, 1000, 0);
                task.into()
            })
            .collect();

        let result = TaskStrategist::build_strategy(tasks, &validator, None);
        assert!(matches!(result, Err(TaskStrategistError::FailedToFit)));
    }

    /// Proves uniqueness no-ops have distinct data and a budget even without settlement tasks.
    #[test]
    fn test_uniqueness_nonce_renders_distinct_noop_instruction() {
        use solana_transaction::versioned::VersionedTransaction;

        let noop_program = solana_pubkey::pubkey!("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV");
        let authority = Keypair::new();
        assert_eq!(TransactionUtils::tasks_compute_units(&[], None), 0);
        assert!(TransactionUtils::tasks_compute_units(&[], Some(1)) > 0);
        let assemble = |nonce: Option<u64>| {
            TransactionUtils::assemble_tasks_tx_with_uniqueness_nonce(
                &authority,
                &[create_test_commit_task(1, 100, 0).into()],
                u64::default(),
                &[],
                nonce,
            )
            .expect("assembles")
        };

        let without_nonce = assemble(None);
        assert!(!without_nonce.message.static_account_keys().contains(&noop_program));

        let extract_noop_data = |tx: &VersionedTransaction| {
            let keys = tx.message.static_account_keys();
            tx.message
                .instructions()
                .iter()
                .find(|ix| keys[ix.program_id_index as usize] == noop_program)
                .expect("noop instruction present")
                .data
                .clone()
        };
        assert_eq!(extract_noop_data(&assemble(Some(1))), 1u64.to_le_bytes());
        assert_eq!(extract_noop_data(&assemble(Some(2))), 2u64.to_le_bytes());
    }

    #[test]
    fn test_optimize_strategy_prioritizes_largest_tasks() {
        let mut tasks: [Task; 3] = [
            create_test_commit_task(1, 100, 0).into(),
            create_test_commit_task(2, 5000, 0).into(), // Larger task
            create_test_commit_task(3, 5000, 0).into(), // Larger task
        ];

        let _ = TaskStrategist::try_optimize_tx_size_if_needed(&mut tasks, None);
        // The larger task should have been optimized first
        assert!(matches!(
            commit_delivery(&tasks[0]),
            CommitDelivery::StateInArgs
        ));
        assert!(matches!(
            commit_delivery(&tasks[1]),
            CommitDelivery::StateInBuffer
        ));
    }

    #[test]
    fn test_mixed_task_types_with_optimization() {
        let validator = Pubkey::new_unique();
        let tasks: Vec<Task> = vec![
            create_test_commit_task(1, 5000, 0).into(),
            create_test_undelegate_task().into(),
            create_test_base_action_task(500).into(),
            create_test_undelegate_task().into(),
        ];

        let strategy =
            TaskStrategist::build_strategy(tasks, &validator, None).expect("Should build strategy");

        assert_eq!(strategy.tasks.len(), 4);

        assert!(matches!(
            strategy.tasks.as_slice(),
            [
                Task::CommitFinalize(task),
                Task::Undelegate(_),
                Task::BaseAction(_),
                Task::Undelegate(_),
            ] if matches!(task.delivery, CommitDelivery::StateInBuffer)
        ));
        assert!(strategy.lookup_tables_keys.is_empty());
    }

    #[tokio::test]
    async fn test_follow_up_tasks_include_request_for_owner_program_undelegate() {
        let delegated_account = Pubkey::new_unique();
        let intent = create_test_intent_bundle(0, &[], &[delegated_account]).inner;
        let info_provider = Arc::new(MockInfoProvider {
            delegation_metadata: HashMap::from([(
                delegated_account,
                (UndelegationRequester::OwnerProgram, delegated_account),
            )]),
            ..Default::default()
        });

        let tasks = TaskBuilder::follow_up_tasks(&info_provider, &intent).await.unwrap();

        let Task::Undelegate(task) = &tasks[0] else {
            panic!("expected undelegate task");
        };
        assert_eq!(task.delegated_account, delegated_account);
        assert_eq!(task.rent_reimbursement, delegated_account);
        assert!(task.include_undelegation_request);
    }

    #[tokio::test]
    async fn test_all_commit_intents_use_combined_tasks() {
        let pubkey = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        for use_diff in [false, true] {
            for combined in [false, true] {
                for undelegate in [false, true] {
                    let mut intent = create_test_intent_bundle(0, &[pubkey], &[]).inner;
                    let mut accounts = intent
                        .intent_bundle
                        .commit
                        .take()
                        .unwrap()
                        .get_committed_accounts()
                        .clone();
                    accounts[0].account.data = vec![1; if use_diff { 1024 } else { 40 }];
                    let mut base = accounts[0].account.clone();
                    base.data[0] = 0;
                    let info_provider = Arc::new(MockInfoProvider {
                        next_commit_id: Some(0),
                        base_accounts: if use_diff {
                            HashMap::from([(pubkey, Some(base))])
                        } else {
                            HashMap::new()
                        },
                        ..Default::default()
                    });
                    let BaseActionTask::V1(action) = create_test_base_action_task(0) else {
                        panic!("expected v1 action");
                    };
                    let commit = CommitType::WithBaseActions {
                        committed_accounts: accounts,
                        base_actions: vec![action.action.clone()],
                    };
                    let base_intent = match (combined, undelegate) {
                        (false, false) => MagicBaseIntent::Commit(commit),
                        (true, false) => MagicBaseIntent::CommitFinalize(commit),
                        (false, true) => {
                            MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                                commit_action: commit,
                                undelegate_action: UndelegateType::WithBaseActions(vec![
                                    action.action,
                                ]),
                            })
                        }
                        (true, true) => {
                            MagicBaseIntent::CommitFinalizeAndUndelegate(CommitAndUndelegate {
                                commit_action: commit,
                                undelegate_action: UndelegateType::WithBaseActions(vec![
                                    action.action,
                                ]),
                            })
                        }
                    };
                    intent.intent_bundle = base_intent.into();
                    let mut commits =
                        TaskBuilder::commit_tasks(&info_provider, &intent).await.unwrap();
                    let follow_ups =
                        TaskBuilder::follow_up_tasks(&info_provider, &intent).await.unwrap();
                    assert_eq!(commits.len(), 1 + usize::from(combined));
                    let Task::CommitFinalize(task) = &mut commits[0] else {
                        panic!("all commit intents must use CommitFinalize");
                    };
                    assert_eq!(task.allow_undelegation, undelegate);
                    assert_eq!(task.committed_account.pubkey, pubkey);
                    assert_eq!(
                        matches!(task.delivery, CommitDelivery::DiffInArgs { .. }),
                        use_diff
                    );
                    let inline_ix = task.instruction(&authority);
                    assert!(task.try_use_buffer());
                    let buffer_ix = task.instruction(&authority);
                    assert!(inline_ix.data.starts_with(&DlpDiscriminator::CommitFinalize.to_vec()));
                    assert!(
                        buffer_ix
                            .data
                            .starts_with(&DlpDiscriminator::CommitFinalizeFromBuffer.to_vec())
                    );
                    assert_eq!(inline_ix.program_id, dpi::id());
                    assert_eq!(buffer_ix.program_id, dpi::id());
                    assert_eq!(
                        follow_ups.len(),
                        usize::from(!combined) + 2 * usize::from(undelegate)
                    );
                    let mut tail = follow_ups.iter();
                    if !combined {
                        assert!(matches!(tail.next(), Some(Task::BaseAction(_))));
                    }
                    if undelegate {
                        assert!(matches!(tail.next(), Some(Task::Undelegate(_))));
                        assert!(matches!(tail.next(), Some(Task::BaseAction(_))));
                    }
                    assert!(tail.next().is_none());
                }
            }
        }
    }

    #[tokio::test]
    async fn test_combined_commit_delivery_formats_without_follow_up() {
        for data_len in [40, 10_240] {
            for use_alts in [false, true] {
                let pubkey = Pubkey::new_unique();
                let mut intent = create_test_intent_bundle(0, &[pubkey], &[]).inner;
                intent.intent_bundle.commit.as_mut().unwrap().get_committed_accounts_mut()[0]
                    .account
                    .data = vec![1; data_len];
                let BaseActionTask::V1(mut action) = create_test_base_action_task(0) else {
                    panic!("expected v1 action");
                };
                if use_alts {
                    action.action.account_metas_per_program = (0..65)
                        .map(|_| ShortAccountMeta {
                            pubkey: Pubkey::new_unique(),
                            is_writable: false,
                        })
                        .collect();
                }
                intent.intent_bundle.standalone_actions.push(action.action);
                let provider = Arc::new(MockInfoProvider {
                    next_commit_id: Some(0),
                    base_accounts: HashMap::from([(pubkey, None)]),
                    ..Default::default()
                });
                let commits = TaskBuilder::commit_tasks(&provider, &intent).await.unwrap();
                let follow_ups = TaskBuilder::follow_up_tasks(&provider, &intent).await.unwrap();
                assert!(follow_ups.is_empty());
                let plan =
                    TaskStrategist::build_plan(commits, follow_ups, &Pubkey::new_unique(), Some(1))
                        .unwrap();
                assert!(plan.follow_up.is_none());
                let strategy = plan.current;
                assert_eq!(strategy.uses_alts(), use_alts);
                let Task::CommitFinalize(task) = &strategy.tasks[1] else {
                    panic!("format fallback must retain CommitFinalize");
                };
                assert_eq!(
                    matches!(task.delivery, CommitDelivery::StateInBuffer),
                    data_len > 4096
                );
            }
        }
    }

    #[tokio::test]
    async fn test_build_split_plan_when_combined_compute_budget_exceeded() {
        let pubkeys: [Pubkey; 6] = std::array::from_fn(|_| Pubkey::new_unique());
        let intent = create_test_intent_bundle(0, &[], &pubkeys).inner;
        let info_provider = Arc::new(MockInfoProvider {
            next_commit_id: Some(0),
            ..Default::default()
        });
        let commit_tasks = TaskBuilder::commit_tasks(&info_provider, &intent).await.unwrap();
        let follow_up_tasks = TaskBuilder::follow_up_tasks(&info_provider, &intent).await.unwrap();

        // Six commits and six undelegations need 1.44M CU together;
        // each group fits separately with 720K CU.
        let plan =
            TaskStrategist::build_plan(commit_tasks, follow_up_tasks, &Pubkey::new_unique(), None)
                .expect("Both transactions fit separately");

        assert_eq!(plan.current.tasks.len(), 6);
        assert_eq!(plan.follow_up.unwrap().tasks.len(), 6);
    }
}
