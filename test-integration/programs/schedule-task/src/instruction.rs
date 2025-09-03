use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::delegate_args::{
    DelegateAccountMetas, DelegateAccounts,
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::state::Counter;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct ScheduleArgs {
    pub task_id: u64,
    pub execution_interval_millis: u64,
    pub n_executions: u64,
    pub error: bool,
    pub signer: bool,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct CancelArgs {
    pub task_id: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct DelegateArgs {
    pub valid_until: i64,
    pub commit_frequency_ms: u32,
}

pub const MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE: u16 = 10_240;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub enum ScheduleTaskInstruction {
    /// Creates a counter PDA.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that is creating the account.
    /// 1. `[write]` The counter PDA account that will be created.
    /// 2. `[]` The system program account.
    InitCounter,

    /// Schedules a task to increase the counter.
    ///
    /// Accounts:
    /// 0. `[]`       Magic Program account.
    /// 1. `[signer]` The payer that created and is scheduling the task.
    /// 2. `[write]`  Task context account.
    /// 3. `[signer]` The counter PDA account whose size we are increasing.
    Schedule(ScheduleArgs),

    /// Schedules a task to increase the counter.
    ///
    /// Accounts:
    /// 0. `[]`       Magic program account.
    /// 1. `[signer]` The payer that created and is cancelling the task.
    /// 2. `[write]`  Task context account.
    Cancel(CancelArgs),

    /// Increments the counter.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created the account.
    /// 1. `[write]` The counter PDA account that will be updated.
    IncrementCounter,

    /// Increments the counter.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created the account.
    /// 1. `[signer, write]` The counter PDA account that will be updated.
    IncrementCounterSigned,

    /// Increments the counter with an error.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created the account.
    /// 1. `[write]` The counter PDA account that will be updated.
    IncrementCounterError,

    /// Delegates the counter account to an ephemaral validator
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that is delegating the account.
    /// 1. `[write]` The counter PDA account that will be delegated.
    /// 2. `[]` The owner program of the delegated account
    /// 3. `[write]` The buffer account of the delegated account
    /// 4. `[write]` The delegation record account of the delegated account
    /// 5. `[write]` The delegation metadata account of the delegated account
    /// 6. `[]` The delegation program
    /// 7. `[]` The system program
    Delegate(DelegateArgs),
}

pub fn create_init_counter_ix(payer: Pubkey) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = Counter::pda(&payer);
    let accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &ScheduleTaskInstruction::InitCounter,
        accounts,
    )
}

pub fn create_increment_counter_ix(payer: Pubkey) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = Counter::pda(&payer);
    let accounts = vec![
        AccountMeta::new_readonly(payer, true),
        AccountMeta::new(pda, false),
    ];

    Instruction::new_with_borsh(
        *program_id,
        &ScheduleTaskInstruction::IncrementCounter,
        accounts,
    )
}

pub fn create_increment_counter_signed_ix(payer: Pubkey) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = Counter::pda(&payer);
    let accounts = vec![
        AccountMeta::new_readonly(payer, true),
        AccountMeta::new(pda, true),
    ];

    Instruction::new_with_borsh(
        *program_id,
        &ScheduleTaskInstruction::IncrementCounterSigned,
        accounts,
    )
}

pub fn create_increment_counter_error_ix(payer: Pubkey) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = Counter::pda(&payer);
    let accounts = vec![
        AccountMeta::new_readonly(payer, true),
        AccountMeta::new(pda, false),
    ];

    Instruction::new_with_borsh(
        *program_id,
        &ScheduleTaskInstruction::IncrementCounterError,
        accounts,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_schedule_task_ix(
    payer: Pubkey,
    task_context: Pubkey,
    magic_program: Pubkey,
    task_id: u64,
    execution_interval_millis: u64,
    n_executions: u64,
    error: bool,
    signer: bool,
) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = Counter::pda(&payer);
    let accounts = vec![
        AccountMeta::new_readonly(magic_program, false),
        AccountMeta::new(payer, true),
        AccountMeta::new(task_context, false),
        AccountMeta::new(pda, false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &ScheduleTaskInstruction::Schedule(ScheduleArgs {
            task_id,
            execution_interval_millis,
            n_executions,
            error,
            signer,
        }),
        accounts,
    )
}

pub fn create_cancel_task_ix(
    payer: Pubkey,
    task_context: Pubkey,
    magic_program: Pubkey,
    task_id: u64,
) -> Instruction {
    let program_id = &crate::id();
    let accounts = vec![
        AccountMeta::new_readonly(magic_program, false),
        AccountMeta::new(payer, true),
        AccountMeta::new(task_context, false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &ScheduleTaskInstruction::Cancel(CancelArgs { task_id }),
        accounts,
    )
}

pub fn create_delegate_ix(payer: Pubkey) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = Counter::pda(&payer);

    let delegate_accounts = DelegateAccounts::new(pda, *program_id);
    let delegate_metas = DelegateAccountMetas::from(delegate_accounts);
    let account_metas = vec![
        AccountMeta::new(payer, true),
        delegate_metas.delegated_account,
        delegate_metas.owner_program,
        delegate_metas.delegate_buffer,
        delegate_metas.delegation_record,
        delegate_metas.delegation_metadata,
        delegate_metas.delegation_program,
        delegate_metas.system_program,
    ];

    let args = DelegateArgs {
        valid_until: i64::MAX,
        commit_frequency_ms: 1_000_000_000,
    };

    Instruction::new_with_borsh(
        *program_id,
        &ScheduleTaskInstruction::Delegate(args),
        account_metas,
    )
}
