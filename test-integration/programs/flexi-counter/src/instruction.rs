use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::{
    consts::{MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    delegate_args::{DelegateAccountMetas, DelegateAccounts},
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::state::FlexiCounter;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct DelegateArgs {
    pub valid_until: i64,
    pub commit_frequency_ms: u32,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct ScheduleArgs {
    pub task_id: i64,
    pub execution_interval_millis: i64,
    pub iterations: i64,
    pub error: bool,
    pub signer: bool,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct CancelArgs {
    pub task_id: i64,
}

pub const MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE: u16 = 10_240;

/// The counter has both mul and add instructions in order to facilitate tests where
/// order matters. For example in the case of the following operations:
/// +4, *2
/// if the *2 operation runs before the add then we end up with 4 as a result instead of
/// the correct result 8.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub enum FlexiCounterInstruction {
    /// Creates a FlexiCounter account.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that is creating the account.
    /// 1. `[write]` The counter PDA account that will be created.
    /// 2. `[]` The system program account.
    Init { label: String, bump: u8 },

    /// Increases the size of the FlexiCounter to reach the given bytes.
    /// Max increase is [MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE] per instruction
    /// which means this instruction needs to be called multiple times to reach
    /// the desired size.
    ///
    /// NOTE: that the account needs to be funded for the full desired account size
    ///       via an airdrop after [FlexiCounterInstruction::Init].
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created and is resizing the account.
    /// 1. `[write]` The counter PDA account whose size we are increasing.
    /// 2. `[]` The system program account.
    Realloc {
        /// The target size we try to resize to.
        bytes: u64,
        /// The count of invocations of realloc that this instruction represents.
        invocation_count: u16,
    },

    /// Updates the FlexiCounter by adding the count to it.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created the account.
    /// 1. `[write]` The counter PDA account that will be updated.
    Add { count: u8 },

    /// Updates the FlexiCounter by adding the count to it without a signer.
    ///
    /// Accounts:
    /// 0. `[write]` The counter PDA account that will be updated.
    AddUnsigned { count: u8 },

    /// Adds the count to the counter with an error.
    ///
    /// Accounts:
    /// 0. `[write]` The counter PDA account that will be updated.
    AddError { count: u8 },

    /// Updates the FlexiCounter by multiplying  the count with the multiplier.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created the account.
    /// 1. `[write]` The counter PDA account that will be updated.
    Mul { multiplier: u8 },

    /// Delegates the FlexiCounter account to an ephemaral validator
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

    /// Updates the FlexiCounter by adding the count to it and then
    /// commits its current state, optionally undelegating the account.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created the account.
    /// 1. `[write]`  The counter PDA account that will be updated.
    /// 2. `[]`       MagicContext (used to record scheduled commit)
    /// 3. `[]`       MagicBlock Program (used to schedule commit)
    AddAndScheduleCommit { count: u8, undelegate: bool },

    /// Updates the first FlexiCounter by adding the count found in the
    /// second FlexiCounter created by another payer
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created the first account.
    /// 1. `[write]`  The target PDA account of the payer that will be updated.
    /// 2. `[]`  The source PDA account whose count will be added.
    AddCounter,

    /// Creates intent that will schedule intent with some action
    /// Actions will call back our program
    ///
    /// Accounts:
    /// 0.      `[]`       Destination program
    /// 1.      `[]`       MagicContext (used to record scheduled commit)
    /// 2.      `[]`       MagicBlock Program (used to schedule commit)
    /// 3.      `[write]`  Transfer destination during action
    /// 4.      `[]`       system program
    /// 5.      `[signer]` Escrow authority
    /// ...
    /// 5+n-1   `[signer]` Escrow authority`
    /// 5+n     `[write]`  Counter pda
    /// ...
    /// 5+2n    `[write]`  Counter pda
    CreateIntent {
        num_committees: u8,
        counter_diffs: Vec<i64>,
        is_undelegate: bool,
        compute_units: u32,
    },

    /// Handler for scheduled action after commit.
    ///
    /// Accounts (order must match handler below):
    /// 0. `[]`       Escrow authority (creator) – unchecked
    /// 1. `[signer]` Escrow account (must sign; escrow PDA)
    /// 2. `[]`       Delegated account (must be owned by ER during commit)
    /// 3. `[write]`  Destination account to receive lamports
    /// 4. `[]`       System program
    CommitActionHandler { amount: u64 },

    /// (Optional) Handler for scheduled action after undelegate.
    ///
    /// Accounts (order must match handler below):
    /// 0. `[]`       Escrow authority (creator) – unchecked
    /// 1. `[signer]` Escrow account (must sign; escrow PDA)
    /// 2. `[]`       Previously delegated account (must NOT be owned by ER)
    /// 3. `[write]`  Destination account to receive lamports
    /// 4. `[write]`  Counter account (your FlexiCounter)
    /// 5. `[]`       System program
    UndelegateActionHandler { amount: u64, counter_diff: i64 },

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
}

pub fn create_init_ix(payer: Pubkey, label: String) -> Instruction {
    let program_id = &crate::id();
    let (pda, bump) = FlexiCounter::pda(&payer);
    let accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Init { label, bump },
        accounts,
    )
}

pub fn create_realloc_ix(
    payer: Pubkey,
    bytes: u64,
    invocation_count: u16,
) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Realloc {
            bytes,
            invocation_count,
        },
        accounts,
    )
}

pub fn create_add_ix(payer: Pubkey, count: u8) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts = vec![
        AccountMeta::new_readonly(payer, true),
        AccountMeta::new(pda, false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Add { count },
        accounts,
    )
}

pub fn create_add_unsigned_ix(payer: Pubkey, count: u8) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts = vec![AccountMeta::new(pda, false)];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::AddUnsigned { count },
        accounts,
    )
}

pub fn create_add_error_ix(payer: Pubkey, count: u8) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts = vec![AccountMeta::new(pda, false)];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::AddError { count },
        accounts,
    )
}

pub fn create_mul_ix(payer: Pubkey, multiplier: u8) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts =
        vec![AccountMeta::new(payer, true), AccountMeta::new(pda, false)];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Mul { multiplier },
        accounts,
    )
}

pub fn create_delegate_ix(payer: Pubkey) -> Instruction {
    create_delegate_ix_with_commit_frequency_ms(payer, 0)
}

pub fn create_delegate_ix_with_commit_frequency_ms(
    payer: Pubkey,
    commit_frequency_ms: u32,
) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);

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
        commit_frequency_ms,
    };

    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Delegate(args),
        account_metas,
    )
}

pub fn create_add_and_schedule_commit_ix(
    payer: Pubkey,
    count: u8,
    undelegate: bool,
) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new(MAGIC_CONTEXT_ID, false),
        AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::AddAndScheduleCommit { count, undelegate },
        accounts,
    )
}

pub fn create_add_counter_ix(
    payer: Pubkey,
    source_payer: Pubkey,
) -> Instruction {
    let program_id = &crate::id();
    let (pda_main, _) = FlexiCounter::pda(&payer);
    let (pda_source, _) = FlexiCounter::pda(&source_payer);
    let accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(pda_main, false),
        AccountMeta::new_readonly(pda_source, false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::AddCounter,
        accounts,
    )
}

pub fn create_intent_single_committee_ix(
    payer: Pubkey,
    transfer_destination: Pubkey,
    counter_diff: i64,
    is_undelegate: bool,
    compute_units: u32,
) -> Instruction {
    let program_id = &crate::id();
    let (counter, _) = FlexiCounter::pda(&payer);
    let accounts = vec![
        AccountMeta::new_readonly(crate::id(), false),
        AccountMeta::new(MAGIC_CONTEXT_ID, false),
        AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
        AccountMeta::new(transfer_destination, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new(payer, true),
        AccountMeta::new(counter, false),
    ];

    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::CreateIntent {
            num_committees: 1,
            // Has no effect in non-undelegate case
            counter_diffs: vec![counter_diff],
            is_undelegate,
            compute_units,
        },
        accounts,
    )
}

pub fn create_intent_ix(
    payers: Vec<Pubkey>,
    transfer_destination: Pubkey,
    counter_diffs: Option<Vec<i64>>,
    compute_units: u32,
) -> Instruction {
    let program_id = &crate::id();

    let (is_undelegate, counter_diffs) =
        if let Some(counter_diffs) = counter_diffs {
            (true, counter_diffs)
        } else {
            (false, vec![])
        };
    let payers_meta = payers
        .iter()
        .map(|payer| AccountMeta::new_readonly(*payer, true));
    let counter_metas = payers
        .iter()
        .map(|payer| AccountMeta::new(FlexiCounter::pda(payer).0, false));
    let mut accounts = vec![
        AccountMeta::new_readonly(crate::id(), false),
        AccountMeta::new(MAGIC_CONTEXT_ID, false),
        AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
        AccountMeta::new_readonly(transfer_destination, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    accounts.extend(payers_meta);
    accounts.extend(counter_metas);

    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::CreateIntent {
            num_committees: payers.len() as u8,
            // Has no effect in non-undelegate case
            counter_diffs,
            is_undelegate,
            compute_units,
        },
        accounts,
    )
}

pub fn create_schedule_task_ix(
    payer: Pubkey,
    task_id: i64,
    execution_interval_millis: i64,
    iterations: i64,
    error: bool,
    signer: bool,
) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts = vec![
        AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
        AccountMeta::new(payer, true),
        AccountMeta::new(pda, false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Schedule(ScheduleArgs {
            task_id,
            execution_interval_millis,
            iterations,
            error,
            signer,
        }),
        accounts,
    )
}

pub fn create_cancel_task_ix(payer: Pubkey, task_id: i64) -> Instruction {
    let program_id = &crate::id();
    let accounts = vec![
        AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
        AccountMeta::new(payer, true),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Cancel(CancelArgs { task_id }),
        accounts,
    )
}
