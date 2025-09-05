use integration_test_tools::run_test;
use log::*;
use magicblock_magic_program_api;
use program_schedulecommit::api::schedule_commit_with_payer_cpi_instruction;
use schedulecommit_client::{verify, ScheduleCommitTestContextFields};
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{signer::Signer, transaction::Transaction};
use test_tools_core::init_logger;
use utils::{
    assert_two_committees_synchronized_count,
    assert_two_committees_were_committed,
    get_context_with_delegated_committees,
};

use crate::utils::{
    assert_feepayer_was_committed,
    get_context_with_delegated_committees_without_payer_escrow,
};

mod utils;

#[test]
fn test_committing_fee_payer_without_escrowing_lamports() {
    // NOTE: this test requires the following config
    //   [validator]
    //   base_fees = 1000
    // see ../../../configs/schedulecommit-conf-fees.ephem.toml
    run_test!({
        let ctx = get_context_with_delegated_committees_without_payer_escrow(2);

        let ScheduleCommitTestContextFields {
            payer,
            committees,
            commitment,
            ephem_client,
            ephem_blockhash,
            ..
        } = ctx.fields();

        let ix = schedule_commit_with_payer_cpi_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        );

        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            *ephem_blockhash,
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

        assert!(res.is_err());
        assert!(res
            .err()
            .unwrap()
            .to_string()
            .contains("DoesNotHaveEscrowAccount"));
    });
}

#[test]
fn test_committing_fee_payer_escrowing_lamports() {
    run_test!({
        let ctx = get_context_with_delegated_committees(2);

        let ScheduleCommitTestContextFields {
            payer,
            committees,
            commitment,
            ephem_client,
            ephem_blockhash,
            ..
        } = ctx.fields();

        let ix = schedule_commit_with_payer_cpi_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        );

        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            *ephem_blockhash,
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
        assert!(res.is_ok());

        let res = verify::fetch_and_verify_commit_result_from_logs(&ctx, *sig);
        assert_two_committees_were_committed(&ctx, &res, true);
        assert_two_committees_synchronized_count(&ctx, &res, 1);

        // The fee payer should have been committed
        assert_feepayer_was_committed(&ctx, &res, true);
    });
}
