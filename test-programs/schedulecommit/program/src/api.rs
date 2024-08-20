use borsh::BorshSerialize;
use ephemeral_rollups_sdk::delegate_args::{
    DelegateAccountMetas, DelegateAccounts,
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::{DelegateCpiArgs, DELEGATE_CPI_IX, INIT_IX, SCHEDULECOMMIT_CPI_IX};

pub fn init_account_instruction(
    payer: Pubkey,
    committee: Pubkey,
) -> Instruction {
    let program_id = crate::id();
    let account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(committee, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    let instruction_data = vec![INIT_IX];
    Instruction::new_with_bytes(program_id, &instruction_data, account_metas)
}

pub fn delegate_account_cpi_instruction(player: Pubkey) -> Instruction {
    let program_id = crate::id();
    let (pda, _) = pda_and_bump(&player);

    let args = DelegateCpiArgs {
        valid_until: i64::MAX,
        commit_frequency_ms: 1_000_000_000,
        player,
    };

    let delegate_accounts = DelegateAccounts::new(pda, program_id);
    let delegate_metas = DelegateAccountMetas::from(delegate_accounts);
    let account_metas = vec![
        AccountMeta::new(player, true),
        delegate_metas.delegate_account,
        delegate_metas.owner_program,
        delegate_metas.buffer,
        delegate_metas.delegation_record,
        delegate_metas.delegation_metadata,
        delegate_metas.delegation_program,
        delegate_metas.system_program,
    ];

    let mut instruction_data = args.try_to_vec().unwrap();
    instruction_data.insert(0, DELEGATE_CPI_IX);
    Instruction::new_with_bytes(program_id, &instruction_data, account_metas)
}

/// Creates an instruction that calls the _legit_ program which owns
/// the PDAs to be commited via CPI into the MagicBlock program.
/// It provides the following account metas to the invoked program:
///
/// - `[WRITE, SIGNER]` Payer
/// - `[]` MagicBlock Program
///
/// If this is invoked directly then no other accounts are needed.
/// However if this is invoked via CPI then the wrapping instruction will need the
/// [crate::id()] to be provided as part of the account metadata.
pub fn schedule_commit_cpi_instruction(
    payer: Pubkey,
    magic_program_id: Pubkey,
    players: &[Pubkey],
    committees: &[Pubkey],
) -> Instruction {
    let program_id = crate::id();
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(magic_program_id, false),
    ];
    for committee in committees {
        account_metas.push(AccountMeta::new(*committee, false));
    }

    let mut instruction_data = vec![SCHEDULECOMMIT_CPI_IX];
    for player in players {
        instruction_data.extend_from_slice(player.as_ref());
    }
    Instruction::new_with_bytes(program_id, &instruction_data, account_metas)
}

// -----------------
// PDA
// -----------------
const ACCOUNT: &str = "magic_schedule_commit";

pub fn pda_seeds(acc_id: &Pubkey) -> [&[u8]; 2] {
    [ACCOUNT.as_bytes(), acc_id.as_ref()]
}

pub fn pda_seeds_with_bump<'a>(
    acc_id: &'a Pubkey,
    bump: &'a [u8; 1],
) -> [&'a [u8]; 3] {
    [ACCOUNT.as_bytes(), acc_id.as_ref(), bump]
}

pub fn pda_seeds_vec_with_bump(acc_id: Pubkey, bump: u8) -> Vec<Vec<u8>> {
    let mut seeds = vec![ACCOUNT.as_bytes().to_vec()];
    seeds.push(acc_id.as_ref().to_vec());
    seeds.push(vec![bump]);
    seeds
}

pub fn pda_and_bump(acc_id: &Pubkey) -> (Pubkey, u8) {
    let program_id = crate::id();
    let seeds = pda_seeds(acc_id);
    Pubkey::find_program_address(&seeds, &program_id)
}
