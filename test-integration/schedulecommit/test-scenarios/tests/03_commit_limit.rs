use std::sync::OnceLock;

use integration_test_tools::run_test;
use magicblock_program::magic_sys::{COMMIT_LIMIT, COMMIT_LIMIT_ERR};
use program_schedulecommit::{
    api::{
        schedule_commit_and_undelegate_cpi_instruction,
        schedule_commit_cpi_instruction, UserSeeds,
    },
    ScheduleCommitType,
};
use schedulecommit_client::{
    verify, ScheduleCommitTestContext, ScheduleCommitTestContextFields,
};
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    instruction::InstructionError, signer::Signer, transaction::Transaction,
};
use test_kit::init_logger;
use tracing::*;
use utils::{
    assert_account_was_undelegated_on_chain, assert_committee_was_committed,
    assert_is_instruction_error, extract_transaction_error,
    get_context_with_delegated_committees,
};

mod utils;

// ---------------------------------------------------------------------------
// Shared prepared context
//
// One context holding NUM_TESTS committees. All accounts are committed
// COMMIT_LIMIT times together (one tx per round, all PDAs in each tx) so
// they sit at the boundary of failure. Each test then uses its own committee
// slot independently.
//
// OnceLock ensures preparation runs exactly once regardless of which test
// triggers it first; the other test blocks until the pool is ready.
// ---------------------------------------------------------------------------

const NUM_TESTS: usize = 2;
const IDX_COMMIT_FAILS: usize = 0;
const IDX_UNDELEGATE_SUCCEEDS: usize = 1;

static PREPARED: OnceLock<ScheduleCommitTestContext> = OnceLock::new();

fn get_prepared() -> &'static ScheduleCommitTestContext {
    PREPARED.get_or_init(|| {
        let ctx = get_context_with_delegated_committees(
            NUM_TESTS,
            UserSeeds::MagicScheduleCommit,
        );

        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();

        let players: Vec<_> =
            committees.iter().map(|(p, _)| p.pubkey()).collect();
        let pdas: Vec<_> = committees.iter().map(|(_, pda)| *pda).collect();

        for n in 0..COMMIT_LIMIT {
            let ix = schedule_commit_cpi_instruction(
                payer.pubkey(),
                magicblock_magic_program_api::id(),
                magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
                &players,
                &pdas,
                ScheduleCommitType::Commit,
            );
            let blockhash = ephem_client.get_latest_blockhash().unwrap();
            let tx = Transaction::new_signed_with_payer(
                &[ix],
                Some(&payer.pubkey()),
                &[&payer],
                blockhash,
            );
            let sig = *tx.get_signature();
            ephem_client
                .send_and_confirm_transaction_with_spinner_and_config(
                    &tx,
                    *commitment,
                    RpcSendTransactionConfig {
                        skip_preflight: true,
                        ..Default::default()
                    },
                )
                .unwrap_or_else(|e| {
                    panic!(
                        "prepare commit {}/{} failed: {:?}",
                        n + 1,
                        COMMIT_LIMIT,
                        e
                    )
                });
            info!("prepare commit {}/{}: {}", n + 1, COMMIT_LIMIT, sig);
            verify::fetch_and_verify_commit_result_from_logs(&ctx, sig);
        }

        ctx
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_schedule_commit_fails_at_commit_limit() {
    run_test!({
        let ctx = get_prepared();
        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();

        let committee = &committees[IDX_COMMIT_FAILS];
        let ix = schedule_commit_cpi_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &[committee.0.pubkey()],
            &[committee.1],
            ScheduleCommitType::Commit,
        );
        let blockhash = ephem_client.get_latest_blockhash().unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
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
            InstructionError::Custom(COMMIT_LIMIT_ERR),
        );
    });
}

#[test]
fn test_schedule_commit_and_undelegate_succeeds_at_commit_limit() {
    run_test!({
        let ctx = get_prepared();
        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();

        let committee = &committees[IDX_UNDELEGATE_SUCCEEDS];
        let ix = schedule_commit_and_undelegate_cpi_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &[committee.0.pubkey()],
            &[committee.1],
        );
        let blockhash = ephem_client.get_latest_blockhash().unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            blockhash,
        );

        let sig = *tx.get_signature();
        ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("undelegate at limit failed: {:?}", e));

        // verify via logs using a single-committee context view
        let res = verify::fetch_and_verify_commit_result_from_logs(ctx, sig);
        assert_committee_was_committed(committee.1, &res, true);
        assert_account_was_undelegated_on_chain(
            ctx,
            committee.1,
            program_schedulecommit::id(),
        );
    });
}
