use std::fmt::Debug;

use magicblock_program::magic_scheduled_base_intent::BaseAction;
use solana_pubkey::Pubkey;
use thiserror::Error;

pub mod task;
pub mod task_builder;
pub mod task_strategist;
pub(crate) mod task_visitors;
pub mod utils;
pub mod visitor;

mod buffer_lifecycle;
mod commit_task;
mod commit_task_builder;

pub use buffer_lifecycle::*;
pub use commit_task::*;
pub use commit_task_builder::*;
pub use task::*;
//
// TODO (snawaz): Ideally, TaskType should not exist.
// Instead we should have Task, an enum with all its variants.
//
// Also, instead of TaskStrategy, we can have requires_buffer() -> bool?
//

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskType {
    Commit,
    Finalize,
    Undelegate,
    Action,
}

#[derive(Clone, Debug)]
pub enum PreparationState {
    NotNeeded,
    Required(PreparationTask),
    Cleanup(CleanupTask),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TaskStrategy {
    Args,
    Buffer,
}

#[derive(Debug, Clone)]
pub struct UndelegateTask {
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub rent_reimbursement: Pubkey,
}

#[derive(Debug, Clone)]
pub struct FinalizeTask {
    pub delegated_account: Pubkey,
}

#[derive(Debug, Clone)]
pub struct BaseActionTask {
    pub action: BaseAction,
}

#[derive(Error, Debug)]
pub enum TaskError {
    #[error("Invalid preparation state transition")]
    PreparationStateTransitionError,
}

pub type TaskResult<T> = Result<T, TaskError>;

#[cfg(test)]
mod serialization_safety_test {
    use std::sync::Arc;

    use magicblock_program::{
        args::ShortAccountMeta,
        magic_scheduled_base_intent::{CommittedAccount, ProgramArgs},
    };
    use solana_account::Account;
    use solana_sdk::instruction::Instruction;

    use crate::{
        intent_executor::NullTaskInfoFetcher,
        tasks::{Task, *},
    };

    // Test all ArgsTask variants
    #[tokio::test]
    async fn test_args_task_instruction_serialization() {
        let validator = Pubkey::new_unique();

        // Test Commit variant
        let commit_task = Task::Commit(
            CommitTask::new(
                123,
                true,
                CommittedAccount {
                    pubkey: Pubkey::new_unique(),
                    account: Account {
                        lamports: 1000,
                        data: vec![1, 2, 3],
                        owner: Pubkey::new_unique(),
                        executable: false,
                        rent_epoch: 0,
                    },
                },
                &Arc::new(NullTaskInfoFetcher),
            )
            .await,
        );
        assert_serializable(&commit_task.instruction(&validator));

        // Test Finalize variant
        let finalize_task = Task::Finalize(FinalizeTask {
            delegated_account: Pubkey::new_unique(),
        });
        assert_serializable(&finalize_task.instruction(&validator));

        // Test Undelegate variant
        let undelegate_task = Task::Undelegate(UndelegateTask {
            delegated_account: Pubkey::new_unique(),
            owner_program: Pubkey::new_unique(),
            rent_reimbursement: Pubkey::new_unique(),
        });
        assert_serializable(&undelegate_task.instruction(&validator));

        // Test BaseAction variant
        let base_action = Task::BaseAction(BaseActionTask {
            action: BaseAction {
                destination_program: Pubkey::new_unique(),
                escrow_authority: Pubkey::new_unique(),
                account_metas_per_program: vec![ShortAccountMeta {
                    pubkey: Pubkey::new_unique(),
                    is_writable: true,
                }],
                data_per_program: ProgramArgs {
                    data: vec![4, 5, 6],
                    escrow_index: 1,
                },
                compute_units: 10_000,
            },
        });
        assert_serializable(&base_action.instruction(&validator));
    }

    // Test BufferTask variants
    #[tokio::test]
    async fn test_task_instruction_serialization() {
        let validator = Pubkey::new_unique();

        let task = Task::Commit(
            CommitTask::new(
                456,
                false,
                CommittedAccount {
                    pubkey: Pubkey::new_unique(),
                    account: Account {
                        lamports: 2000,
                        data: vec![7, 8, 9],
                        owner: Pubkey::new_unique(),
                        executable: false,
                        rent_epoch: 0,
                    },
                },
                &Arc::new(NullTaskInfoFetcher),
            )
            .await,
        );
        assert_serializable(&task.instruction(&validator));
    }

    // // Test preparation instructions
    #[tokio::test]
    async fn test_preparation_instructions_serialization() {
        let authority = Pubkey::new_unique();

        // Test buffer strategy preparation
        let task = Task::Commit(
            CommitTask::new(
                789,
                true,
                CommittedAccount {
                    pubkey: Pubkey::new_unique(),
                    account: Account {
                        lamports: 3000,
                        data: vec![0; 1024], // Larger data to test chunking
                        owner: Pubkey::new_unique(),
                        executable: false,
                        rent_epoch: 0,
                    },
                },
                &Arc::new(NullTaskInfoFetcher),
            )
            .await,
        );

        assert_eq!(task.strategy(), TaskStrategy::Args);

        let task = task.try_optimize_tx_size().unwrap();

        assert_eq!(task.strategy(), TaskStrategy::Buffer);

        let lifecycle = task.lifecycle().unwrap();
        let preparation_task = &lifecycle.preparation;

        assert_serializable(&preparation_task.init_instruction(&authority));
        for ix in preparation_task.realloc_instructions(&authority) {
            assert_serializable(&ix);
        }
        for ix in preparation_task.write_instructions(&authority) {
            assert_serializable(&ix);
        }

        let cleanup = &lifecycle.cleanup;
        assert_serializable(&cleanup.instruction(&authority));
    }

    // Helper function to assert serialization succeeds
    fn assert_serializable(ix: &Instruction) {
        bincode::serialize(ix).unwrap_or_else(|e| {
            panic!("Failed to serialize instruction {:?}: {}", ix, e)
        });
    }
}

#[test]
fn test_close_buffer_limit() {
    use solana_sdk::{
        compute_budget::ComputeBudgetInstruction, signature::Keypair,
        signer::Signer, transaction::Transaction,
    };

    use crate::transactions::{
        serialize_and_encode_base64, MAX_ENCODED_TRANSACTION_SIZE,
    };

    let authority = Keypair::new();

    // Budget ixs (fixed)
    let compute_budget_ix =
        ComputeBudgetInstruction::set_compute_unit_limit(30_000);
    let compute_unit_price_ix =
        ComputeBudgetInstruction::set_compute_unit_price(101);

    // Each task unique: commit_id increments; pubkey is new_unique each time
    let base_commit_id = 101u64;
    let ixs_iter = (0..CleanupTask::max_tx_fit_count_with_budget()).map(|i| {
        let task = CleanupTask {
            commit_id: base_commit_id + i as u64,
            pubkey: Pubkey::new_unique(),
        };
        task.instruction(&authority.pubkey())
    });

    let mut ixs: Vec<_> = [compute_budget_ix, compute_unit_price_ix]
        .into_iter()
        .chain(ixs_iter)
        .collect();

    let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
    println!("{}", serialize_and_encode_base64(&tx).len());
    assert!(
        serialize_and_encode_base64(&tx).len() <= MAX_ENCODED_TRANSACTION_SIZE
    );

    // One more unique task should overflow
    let overflow_task = CleanupTask {
        commit_id: base_commit_id
            + CleanupTask::max_tx_fit_count_with_budget() as u64,
        pubkey: Pubkey::new_unique(),
    };
    ixs.push(overflow_task.instruction(&authority.pubkey()));

    let tx = Transaction::new_with_payer(&ixs, Some(&authority.pubkey()));
    assert!(
        serialize_and_encode_base64(&tx).len() > MAX_ENCODED_TRANSACTION_SIZE
    );
}
