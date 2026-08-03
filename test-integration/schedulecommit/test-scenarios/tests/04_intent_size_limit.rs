#![allow(clippy::result_large_err)]

use integration_test_tools::run_test;
use magicblock_program::magic_sys::INTENT_TOO_LARGE_ERR;
use program_schedulecommit::{
    api::{schedule_commit_cpi_instruction, UserSeeds},
    ScheduleCommitType,
};
use schedulecommit_client::{verify, ScheduleCommitTestContextFields};
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    instruction::InstructionError, signature::Signer, transaction::Transaction,
};
use test_kit::init_logger;
use tracing::*;
use utils::{
    assert_is_instruction_error, extract_transaction_error,
    get_context_with_delegated_committees,
};

mod utils;

/// Small enough that the commit and finalize transactions fit even without
/// any size optimization.
const FEW_ACCOUNTS: usize = 3;
/// CommitAndUndelegate won't with more than 11 accs
const TOO_MANY_ACCOUNTS: usize = 12;

fn schedule_commit_ix(
    payer: &solana_sdk::signature::Keypair,
    committees: &[(
        solana_sdk::signature::Keypair,
        solana_sdk::pubkey::Pubkey,
    )],
) -> solana_sdk::instruction::Instruction {
    schedule_commit_cpi_instruction(
        payer.pubkey(),
        magicblock_magic_program_api::id(),
        magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
        None,
        &committees
            .iter()
            .map(|(player, _)| player.pubkey())
            .collect::<Vec<_>>(),
        &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        ScheduleCommitType::CommitAndUndelegate,
    )
}

#[test]
fn test_schedule_commit_with_few_accounts_fits() {
    run_test!({
        let ctx = get_context_with_delegated_committees(
            FEW_ACCOUNTS,
            UserSeeds::MagicScheduleCommit,
        );

        let ScheduleCommitTestContextFields {
            payer_chain: payer,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();

        let ix = schedule_commit_ix(payer, committees);
        let blockhash = ephem_client.get_latest_blockhash().unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[payer],
            blockhash,
        );

        let sig = tx.get_signature();
        let res = ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        info!("{} '{:?}'", sig, res);
        assert!(
            res.is_ok(),
            "Expected schedule commit to succeed: {:?}",
            res
        );

        verify::fetch_and_verify_commit_result_from_logs(&ctx, *sig);
    });
}

#[test]
fn test_schedule_commit_with_too_many_accounts_is_refused() {
    run_test!({
        let ctx = get_context_with_delegated_committees(
            TOO_MANY_ACCOUNTS,
            UserSeeds::MagicScheduleCommit,
        );

        let ScheduleCommitTestContextFields {
            payer_chain: payer,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();

        let ix = schedule_commit_ix(payer, committees);
        let blockhash = ephem_client.get_latest_blockhash().unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[payer],
            blockhash,
        );

        let res = ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );

        let (tx_result_err, tx_err) = extract_transaction_error(res);
        assert_is_instruction_error(
            tx_err.unwrap(),
            &tx_result_err,
            InstructionError::Custom(INTENT_TOO_LARGE_ERR),
        );
    });
}
