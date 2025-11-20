use dlp::args::{DelegateArgs, DelegateEphemeralBalanceArgs};
use ephemeral_rollups_sdk::delegate_args::{
    DelegateAccountMetas, DelegateAccounts,
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::{
    DelegateCpiArgs, ScheduleCommitCpiArgs, ScheduleCommitInstruction,
};

pub fn init_account_instruction(
    payer: Pubkey,
    player: Pubkey,
    committee: Pubkey,
) -> Instruction {
    let program_id = crate::id();
    let account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(player, true),
        AccountMeta::new(committee, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    build_instruction(
        program_id,
        ScheduleCommitInstruction::Init,
        account_metas,
    )
}

pub fn init_payer_escrow(payer: Pubkey) -> [Instruction; 2] {
    let top_up_ix = dlp::instruction_builder::top_up_ephemeral_balance(
        payer,
        payer,
        Some(300_000_000),
        Some(0),
    );
    let delegate_ix = dlp::instruction_builder::delegate_ephemeral_balance(
        payer,
        payer,
        DelegateEphemeralBalanceArgs {
            index: 0,
            delegate_args: DelegateArgs {
                commit_frequency_ms: 0,
                seeds: vec![],
                validator: None,
            },
        },
    );

    [top_up_ix, delegate_ix]
}

pub fn delegate_account_cpi_instruction(
    payer: Pubkey,
    validator: Option<Pubkey>,
    player: Pubkey,
) -> Instruction {
    let program_id = crate::id();
    let (pda, _) = pda_and_bump(&player);

    let args = DelegateCpiArgs {
        valid_until: i64::MAX,
        commit_frequency_ms: 1_000_000_000,
        validator,
        player,
    };

    let delegate_accounts = DelegateAccounts::new(pda, program_id);
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

    build_instruction(
        program_id,
        ScheduleCommitInstruction::DelegateCpi(args),
        account_metas,
    )
}

/// Creates an instruction that calls the _legit_ program which owns
/// the PDAs to be commited via CPI into the MagicBlock program.
/// It provides the following account metas to the invoked program:
///
/// - `[WRITE, SIGNER]` Payer
/// - `[WRITE]` MagicBlock Context
/// - `[]` MagicBlock Program
///
/// If this is invoked directly then no other accounts are needed.
/// However if this is invoked via CPI then the wrapping instruction will need the
/// [crate::id()] to be provided as part of the account metadata.
pub fn schedule_commit_cpi_instruction(
    payer: Pubkey,
    magic_program_id: Pubkey,
    magic_context_id: Pubkey,
    players: &[Pubkey],
    committees: &[Pubkey],
) -> Instruction {
    schedule_commit_cpi_instruction_impl(
        payer,
        magic_program_id,
        magic_context_id,
        players,
        committees,
        ScheduleCommitCpiInstructionImplArgs {
            undelegate: false,
            commit_payer: false,
        },
    )
}

pub fn schedule_commit_with_payer_cpi_instruction(
    payer: Pubkey,
    magic_program_id: Pubkey,
    magic_context_id: Pubkey,
    players: &[Pubkey],
    committees: &[Pubkey],
) -> Instruction {
    schedule_commit_cpi_instruction_impl(
        payer,
        magic_program_id,
        magic_context_id,
        players,
        committees,
        ScheduleCommitCpiInstructionImplArgs {
            undelegate: false,
            commit_payer: true,
        },
    )
}

pub fn schedule_commit_and_undelegate_cpi_instruction(
    payer: Pubkey,
    magic_program_id: Pubkey,
    magic_context_id: Pubkey,
    players: &[Pubkey],
    committees: &[Pubkey],
) -> Instruction {
    schedule_commit_cpi_instruction_impl(
        payer,
        magic_program_id,
        magic_context_id,
        players,
        committees,
        ScheduleCommitCpiInstructionImplArgs {
            undelegate: true,
            commit_payer: false,
        },
    )
}

struct ScheduleCommitCpiInstructionImplArgs {
    undelegate: bool,
    commit_payer: bool,
}

fn schedule_commit_cpi_instruction_impl(
    payer: Pubkey,
    magic_program_id: Pubkey,
    magic_context_id: Pubkey,
    players: &[Pubkey],
    committees: &[Pubkey],
    args: ScheduleCommitCpiInstructionImplArgs,
) -> Instruction {
    let program_id = crate::id();
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(magic_context_id, false),
        AccountMeta::new_readonly(magic_program_id, false),
    ];
    for committee in committees {
        account_metas.push(AccountMeta::new(*committee, false));
    }

    let cpi_args = ScheduleCommitCpiArgs {
        players: players.to_vec(),
        modify_accounts: true,
        undelegate: args.undelegate,
        commit_payer: args.commit_payer,
    };
    let ix = ScheduleCommitInstruction::ScheduleCommitCpi(cpi_args);
    build_instruction(program_id, ix, account_metas)
}

pub fn schedule_commit_and_undelegate_cpi_with_mod_after_instruction(
    payer: Pubkey,
    magic_program_id: Pubkey,
    magic_context_id: Pubkey,
    players: &[Pubkey],
    committees: &[Pubkey],
) -> Instruction {
    let program_id = crate::id();
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(magic_context_id, false),
        AccountMeta::new_readonly(magic_program_id, false),
    ];
    for committee in committees {
        account_metas.push(AccountMeta::new(*committee, false));
    }

    let ix = ScheduleCommitInstruction::ScheduleCommitAndUndelegateCpiModAfter(
        players.to_vec(),
    );
    build_instruction(program_id, ix, account_metas)
}

pub fn increase_count_instruction(committee: Pubkey) -> Instruction {
    let program_id = crate::id();
    let account_metas = vec![AccountMeta::new(committee, false)];
    build_instruction(
        program_id,
        ScheduleCommitInstruction::IncreaseCount,
        account_metas,
    )
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

fn build_instruction(
    program_id: Pubkey,
    instruction: ScheduleCommitInstruction,
    account_metas: Vec<AccountMeta>,
) -> Instruction {
    Instruction::new_with_bytes(
        program_id,
        &borsh::to_vec(&instruction).unwrap(),
        account_metas,
    )
}
