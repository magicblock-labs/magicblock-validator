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

    /// Updates the FlexiCounter by adding the count to it.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that created the account.
    /// 1. `[write]` The counter PDA account that will be updated.
    Add { count: u8 },

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

pub fn create_add_ix(payer: Pubkey, count: u8) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts =
        vec![AccountMeta::new(payer, true), AccountMeta::new(pda, false)];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Add { count },
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
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);

    let delegate_accounts = DelegateAccounts::new(pda, *program_id);
    let delegate_metas = DelegateAccountMetas::from(delegate_accounts);
    let account_metas = vec![
        AccountMeta::new(payer, true),
        delegate_metas.delegate_account,
        delegate_metas.owner_program,
        delegate_metas.buffer,
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
