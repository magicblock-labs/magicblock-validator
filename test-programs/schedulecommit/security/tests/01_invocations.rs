use std::str::FromStr;

use schedulecommit_client::ScheduleCommitTestContext;
use schedulecommit_program::api::schedule_commit_cpi_instruction;
use sleipnir_core::magic_program;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::Signer,
    system_program,
    transaction::Transaction,
};

use crate::utils::{
    create_nested_schedule_cpis_instruction,
    create_sibling_non_cpi_instruction,
    create_sibling_schedule_cpis_instruction,
};
mod utils;

const _PROGRAM_ADDR: &str = "9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY";

fn prepare_ctx_with_account_to_commit() -> ScheduleCommitTestContext {
    let ctx = if std::env::var("RK").is_ok() {
        ScheduleCommitTestContext::new_random_keys(2)
    } else {
        ScheduleCommitTestContext::new(2)
    };
    ctx.init_committees().unwrap();
    ctx.delegate_committees().unwrap();

    ctx
}

fn create_schedule_commit_ix(
    payer: Pubkey,
    validator_id: Pubkey,
    magic_program_key: Pubkey,
    pubkeys: &[Pubkey],
) -> Instruction {
    let instruction_data = vec![1, 0, 0, 0];
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(validator_id, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    for pubkey in pubkeys {
        account_metas.push(AccountMeta {
            pubkey: *pubkey,
            is_signer: false,
            // NOTE: It appears they need to be writable to be properly cloned?
            is_writable: true,
        });
    }
    Instruction::new_with_bytes(
        magic_program_key,
        &instruction_data,
        account_metas,
    )
}

#[test]
fn test_schedule_commit_directly_with_single_ix() {
    let ctx = prepare_ctx_with_account_to_commit();
    let ScheduleCommitTestContext {
        payer,
        commitment,
        committees,
        ephem_blockhash,
        ephem_client,
        validator_identity,
        ..
    } = &ctx;
    let ix = create_schedule_commit_ix(
        payer.pubkey(),
        *validator_identity,
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
        &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    // TODO: assert it fails with fails with the following
    // [
    //   "Program Magic11111111111111111111111111111111111111 invoke [1]",
    //   "ScheduleCommit ERR: failed to find parent program id",
    //   "Program Magic11111111111111111111111111111111111111 failed: invalid instruction data"
    // ]
    eprintln!("Transaction '{:?}' res: '{:?}'", sig, res);
}

#[test]
fn test_schedule_commit_directly_with_commit_ix_sandwiched() {
    let ctx = prepare_ctx_with_account_to_commit();
    let ScheduleCommitTestContext {
        payer,
        commitment,
        committees,
        ephem_blockhash,
        ephem_client,
        validator_identity,
        ..
    } = &ctx;

    // Send money to one of the PDAs since it is delegated and can be cloned
    let (_, rcvr_pda) = committees[0];

    // 1. Transfer to rcvr
    let transfer_ix_1 = solana_sdk::system_instruction::transfer(
        &payer.pubkey(),
        &rcvr_pda,
        1_000_000,
    );

    // 2. Schedule commit
    let ix = create_schedule_commit_ix(
        payer.pubkey(),
        *validator_identity,
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
        &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
    );

    // 3. Transfer to rcvr again
    let transfer_ix_2 = solana_sdk::system_instruction::transfer(
        &payer.pubkey(),
        &rcvr_pda,
        2_000_000,
    );

    let tx = Transaction::new_signed_with_payer(
        &[transfer_ix_1, ix, transfer_ix_2],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    eprintln!("Transaction '{:?}' res: '{:?}'", sig, res);
    // TODO: assert it fails with fails with the following
    // [
    //   "Program Magic11111111111111111111111111111111111111 invoke [1]",
    //   "ScheduleCommit ERR: failed to find parent program id",
    //   "Program Magic11111111111111111111111111111111111111 failed: invalid instruction data"
    // ]
}

#[test]
fn test_schedule_commit_via_direct_and_indirect_cpi_of_other_program() {
    // TODO: figure out issue:
    // > Unknown program 9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY
    // > Program consumed: 50949 of 200000 compute units
    // > Program returned error: "An account required by the instruction is missing"
    let ctx = prepare_ctx_with_account_to_commit();
    let ScheduleCommitTestContext {
        payer,
        commitment,
        committees,
        ephem_blockhash,
        ephem_client,
        validator_identity,
        ..
    } = &ctx;

    let players = &committees
        .iter()
        .map(|(player, _)| player.pubkey())
        .collect::<Vec<_>>();
    let pdas = &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>();

    let ix = create_sibling_schedule_cpis_instruction(
        payer.pubkey(),
        *validator_identity,
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
        pdas,
        players,
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    eprintln!("Transaction '{:?}' res: '{:?}'", sig, res);
}
/*
   A) Malicious Program Instruction (Non CPI)
   B) CPI to the program that owns the PDAs
   C) Malicious Program Instruction
    D) CPI to MagicBlock Program
*/

#[test]
fn test_schedule_commit_via_direct_and_from_other_program_indirect_cpi_including_non_cpi_instruction(
) {
    let ctx = prepare_ctx_with_account_to_commit();
    let ScheduleCommitTestContext {
        payer,
        commitment,
        committees,
        ephem_blockhash,
        ephem_client,
        validator_identity,
        ..
    } = &ctx;

    let players = &committees
        .iter()
        .map(|(player, _)| player.pubkey())
        .collect::<Vec<_>>();
    let pdas = &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>();

    let non_cpi_ix = create_sibling_non_cpi_instruction(payer.pubkey());

    let cpi_ix = schedule_commit_cpi_instruction(
        payer.pubkey(),
        *validator_identity,
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
        players,
        pdas,
    );

    let nested_cpi_ix = create_nested_schedule_cpis_instruction(
        payer.pubkey(),
        *validator_identity,
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
        pdas,
        players,
    );

    let tx = Transaction::new_signed_with_payer(
        &[non_cpi_ix, cpi_ix, nested_cpi_ix],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    eprintln!("Transaction '{:?}' res: '{:?}'", sig, res);

    // TODO: assert fails with
    // "Program Magic11111111111111111111111111111111111111 invoke [2]",
    // "ScheduleCommit: parent program id: 4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr",
    // "ScheduleCommit ERR: account 9jmXmqNCJmrKFAsVYmteT5EFgzRPNY4Sdvo1mwMRudbi needs to be owned by the invoking program 4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr to be committed, but is owned by 9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY",
    // "Program Magic11111111111111111111111111111111111111 failed: Invalid account owner",
    // "Program 4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr consumed 4263 of 592828 compute units",
    // "Program 4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr failed: Invalid account owner"
}
