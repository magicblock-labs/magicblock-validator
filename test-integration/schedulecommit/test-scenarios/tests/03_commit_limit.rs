use std::sync::OnceLock;

use integration_test_tools::run_test;
use magicblock_program::magic_sys::{COMMIT_LIMIT, COMMIT_LIMIT_ERR};
use program_schedulecommit::{
    api::{
        init_order_book_instruction,
        schedule_commit_and_undelegate_cpi_instruction,
        schedule_commit_cpi_instruction,
        schedule_commit_cpi_with_vault_instruction,
        schedule_commit_with_vault_and_order_book_instruction, UserSeeds,
    },
    ScheduleCommitCpiWithVaultArgs, ScheduleCommitWithOrderBookArgs,
};
use schedulecommit_client::{
    verify, ScheduleCommitTestContext, ScheduleCommitTestContextFields,
};
use serial_test::serial;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signature::Signer,
    transaction::Transaction,
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
// Shared context — plain commit limit (COMMIT_LIMIT = 10)
// ---------------------------------------------------------------------------

const NUM_TESTS: usize = 2;
/// Committee whose next plain commit will hit the limit and fail.
const IDX_COMMIT_FAILS: usize = 0;
/// Committee whose next commit+undelegate still succeeds at the limit.
const IDX_UNDELEGATE_SUCCEEDS: usize = 1;

static PREPARED: OnceLock<ScheduleCommitTestContext> = OnceLock::new();

fn get_prepared() -> &'static ScheduleCommitTestContext {
    PREPARED.get_or_init(|| {
        let ctx = get_context_with_delegated_committees(
            NUM_TESTS,
            UserSeeds::MagicScheduleCommit,
        );

        let ScheduleCommitTestContextFields {
            payer_chain: payer,
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
                None,
                &players,
                &pdas,
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
// Tests — plain commit limit (COMMIT_LIMIT = 10)
// ---------------------------------------------------------------------------

#[test]
fn test_schedule_commit_fails_at_commit_limit() {
    run_test!({
        let ctx = get_prepared();
        let ScheduleCommitTestContextFields {
            payer_chain: payer,
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
            None,
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
            payer_chain: payer,
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
            None,
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

        let res = verify::fetch_and_verify_commit_result_from_logs(ctx, sig);
        assert_committee_was_committed(committee.1, &res, true);
        assert_account_was_undelegated_on_chain(
            ctx,
            committee.1,
            program_schedulecommit::id(),
        );
    });
}

// ---------------------------------------------------------------------------
// Shared context — vault / actual commit limit (ACTUAL_COMMIT_LIMIT = 25)
// ---------------------------------------------------------------------------

/// Matches `ACTUAL_COMMIT_LIMIT` in `magic_scheduled_base_intent.rs`.
const ACTUAL_COMMIT_LIMIT: u64 = 25;
/// Matches `COMMIT_FEE_LAMPORTS` in `magic_scheduled_base_intent.rs`.
const COMMIT_FEE_LAMPORTS: u64 = 100_000;

const NUM_VAULT_TESTS: usize = 3;
/// Fresh committee used to verify that a delegated payer without a vault errors.
const IDX_MISSING_VAULT: usize = 0;
/// Fresh committee used to verify that commits within the limit incur no fee.
const IDX_WITHIN_LIMIT: usize = 1;
/// Committee pre-committed exactly ACTUAL_COMMIT_LIMIT times; its next commit
/// crosses the threshold and triggers a COMMIT_FEE_LAMPORTS charge.
const IDX_OVER_LIMIT: usize = 2;

static VAULT_PREPARED: OnceLock<ScheduleCommitTestContext> = OnceLock::new();

fn get_vault_prepared() -> &'static ScheduleCommitTestContext {
    VAULT_PREPARED.get_or_init(|| {
        let ctx = get_context_with_delegated_committees(
            NUM_VAULT_TESTS,
            UserSeeds::MagicScheduleCommit,
        );

        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            validator_identity,
            ..
        } = ctx.fields();

        // Pre-commit IDX_OVER_LIMIT committee exactly ACTUAL_COMMIT_LIMIT times.
        // The nonce will be at the boundary so the very next commit is charged.
        let charged_committee = &committees[IDX_OVER_LIMIT];
        let player = charged_committee.0.pubkey();
        let pda = charged_committee.1;

        for n in 0..ACTUAL_COMMIT_LIMIT {
            let ix = schedule_commit_cpi_with_vault_instruction(
                payer.pubkey(),
                *validator_identity,
                magicblock_magic_program_api::id(),
                magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
                &[pda],
                ScheduleCommitCpiWithVaultArgs {
                    players: vec![player],
                    undelegate: false,
                    has_magic_vault: true,
                },
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
                        "vault prepare commit {}/{} failed: {:?}",
                        n + 1,
                        ACTUAL_COMMIT_LIMIT,
                        e
                    )
                });
            info!(
                "vault prepare commit {}/{}: {}",
                n + 1,
                ACTUAL_COMMIT_LIMIT,
                sig
            );
            verify::fetch_and_verify_commit_result_from_logs(&ctx, sig);
        }

        ctx
    })
}

// ---------------------------------------------------------------------------
// Tests — vault / ACTUAL_COMMIT_LIMIT (25)
// ---------------------------------------------------------------------------

/// A delegated payer without the required fee vault must receive MissingAccount.
#[test]
#[serial]
fn test_payer_delegated_vault_absent_error() {
    run_test!({
        let ctx = get_vault_prepared();
        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            validator_identity,
            ..
        } = ctx.fields();

        let committee = &committees[IDX_MISSING_VAULT];
        let ix = schedule_commit_cpi_with_vault_instruction(
            payer.pubkey(),
            *validator_identity,
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &[committee.1],
            ScheduleCommitCpiWithVaultArgs {
                players: vec![committee.0.pubkey()],
                undelegate: false,
                has_magic_vault: false,
            },
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
            InstructionError::MissingAccount,
        );
    });
}

/// Commits a fresh account with the vault present while within ACTUAL_COMMIT_LIMIT;
/// the payer's ephemeral balance must remain unchanged.
#[test]
#[serial]
fn test_no_fee_charged_within_actual_commit_limit() {
    run_test!({
        let ctx = get_vault_prepared();
        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            validator_identity,
            ..
        } = ctx.fields();

        let committee = &committees[IDX_WITHIN_LIMIT];

        let payer_balance_before =
            ctx.fetch_ephem_account_balance(&payer.pubkey()).unwrap();

        let ix = schedule_commit_cpi_with_vault_instruction(
            payer.pubkey(),
            *validator_identity,
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &[committee.1],
            ScheduleCommitCpiWithVaultArgs {
                players: vec![committee.0.pubkey()],
                undelegate: false,
                has_magic_vault: true,
            },
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
            .unwrap_or_else(|e| panic!("commit within limit failed: {:?}", e));

        verify::fetch_and_verify_commit_result_from_logs(ctx, sig);

        let payer_balance_after =
            ctx.fetch_ephem_account_balance(&payer.pubkey()).unwrap();

        assert_eq!(
            payer_balance_before, payer_balance_after,
            "Payer balance should not change within commit limit \
             (before={}, after={})",
            payer_balance_before, payer_balance_after
        );
    });
}

/// After ACTUAL_COMMIT_LIMIT commits, the next one must deduct COMMIT_FEE_LAMPORTS
/// from the payer and credit the same amount to the fee vault.
#[test]
#[serial]
fn test_fee_charged_and_vault_credited_after_actual_commit_limit() {
    run_test!({
        let ctx = get_vault_prepared();
        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            validator_identity,
            ..
        } = ctx.fields();

        let committee = &committees[IDX_OVER_LIMIT];
        let magic_fee_vault =
            dlp::pda::magic_fee_vault_pda_from_validator(validator_identity);

        let payer_balance_before =
            ctx.fetch_ephem_account_balance(&payer.pubkey()).unwrap();
        let vault_balance_before = ctx
            .fetch_ephem_account_balance(&magic_fee_vault)
            .unwrap_or(0);

        let ix = schedule_commit_cpi_with_vault_instruction(
            payer.pubkey(),
            *validator_identity,
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &[committee.1],
            ScheduleCommitCpiWithVaultArgs {
                players: vec![committee.0.pubkey()],
                undelegate: false,
                has_magic_vault: true,
            },
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
            .unwrap_or_else(|e| panic!("commit over limit failed: {:?}", e));

        verify::fetch_and_verify_commit_result_from_logs(ctx, sig);

        let payer_balance_after =
            ctx.fetch_ephem_account_balance(&payer.pubkey()).unwrap();
        let vault_balance_after = ctx
            .fetch_ephem_account_balance(&magic_fee_vault)
            .unwrap_or(0);

        assert_eq!(
            payer_balance_after,
            payer_balance_before - COMMIT_FEE_LAMPORTS,
            "Payer should be charged {} lamports (before={}, after={})",
            COMMIT_FEE_LAMPORTS,
            payer_balance_before,
            payer_balance_after
        );
        assert_eq!(
            vault_balance_after,
            vault_balance_before + COMMIT_FEE_LAMPORTS,
            "Vault should be credited {} lamports (before={}, after={})",
            COMMIT_FEE_LAMPORTS,
            vault_balance_before,
            vault_balance_after
        );
    });
}

/// Commits a committee via MagicIntentBundleBuilder with vault and a post-commit
/// UpdateOrderBook action; verifies the commit is included in the scheduled bundle.
#[test]
#[serial]
fn test_schedule_commit_with_vault_and_order_book_action() {
    run_test!({
        let ctx = get_vault_prepared();
        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            payer_chain,
            committees,
            commitment,
            ephem_client,
            validator_identity,
            ..
        } = ctx.fields();

        // Re-use the over-limit committee; the fee is charged but that does not
        // affect the correctness of the action execution path.
        let committee = &committees[IDX_OVER_LIMIT];

        // Derive and lazily init the order book PDA on chain.
        let (order_book_pda, _) = Pubkey::find_program_address(
            &[b"order_book", payer_chain.pubkey().as_ref()],
            &program_schedulecommit::id(),
        );
        if ctx.fetch_chain_account(order_book_pda).is_err() {
            let ix = init_order_book_instruction(
                payer_chain.pubkey(),
                payer_chain.pubkey(),
                order_book_pda,
            );
            ctx.send_and_confirm_instructions_with_payer_chain(
                &[ix],
                payer_chain,
            )
            .unwrap_or_else(|e| panic!("init_order_book failed: {:?}", e));
            info!("Initialized order_book: {}", order_book_pda);
        }

        let ix = schedule_commit_with_vault_and_order_book_instruction(
            payer.pubkey(),
            *validator_identity,
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            order_book_pda,
            &[committee.1],
            ScheduleCommitWithOrderBookArgs {
                players: vec![committee.0.pubkey()],
                with_actions: true,
            },
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
                    "schedule_commit_with_vault_and_order_book failed: {:?}",
                    e
                )
            });

        let res = verify::fetch_and_verify_commit_result_from_logs(ctx, sig);
        assert_committee_was_committed(committee.1, &res, true);

        info!(
            "Commit with vault+order_book action scheduled: {}. \
             UpdateOrderBook will be executed on chain by the committor.",
            sig
        );
    });
}
