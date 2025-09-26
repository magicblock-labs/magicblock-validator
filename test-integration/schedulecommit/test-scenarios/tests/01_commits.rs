use integration_test_tools::run_test;
use log::*;
use magicblock_core::magic_program;
use program_schedulecommit::api::schedule_commit_cpi_instruction;
use schedulecommit_client::{verify, ScheduleCommitTestContextFields};
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{signer::Signer, transaction::Transaction};
use test_kit::init_logger;
use utils::{
    assert_one_committee_synchronized_count,
    assert_one_committee_was_committed,
    assert_two_committees_synchronized_count,
    assert_two_committees_were_committed,
    get_context_with_delegated_committees,
};
mod utils;

// NOTE: This and all other schedule commit tests depend on the following accounts
//       loaded in the mainnet cluster, i.e. the solana-test-validator:
//
//  validator:                tEsT3eV6RFCWs1BZ7AXTzasHqTtMnMLCB2tjQ42TDXD
//  protocol fees vault:      7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg
//  validator fees vault:     DUH8h7rYjdTPYyBUEGAUwZv9ffz5wiM45GdYWYzogXjp
//  delegation program:       DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh
//  committor program:        ComtrB2KEaWgXsW1dhr1xYL4Ht4Bjj3gXnnL6KMdABq

#[test]
fn test_committing_one_account() {
    run_test!({
        let ctx = get_context_with_delegated_committees(1);

        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            ephem_blockhash,
            ..
        } = ctx.fields();

        debug!("Context initialized: {ctx}");

        let ix = schedule_commit_cpi_instruction(
            payer.pubkey(),
            magic_program::id(),
            magic_program::MAGIC_CONTEXT_PUBKEY,
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
        debug!("Submitting tx to commit committee {sig}",);
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

        let res = verify::fetch_and_verify_commit_result_from_logs(&ctx, *sig);
        assert_one_committee_was_committed(&ctx, &res, true);
        assert_one_committee_synchronized_count(&ctx, &res, 1);
    });
}

#[test]
fn test_committing_two_accounts() {
    run_test!({
        let ctx = get_context_with_delegated_committees(2);

        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            ephem_blockhash,
            ..
        } = ctx.fields();

        let ix = schedule_commit_cpi_instruction(
            payer.pubkey(),
            magic_program::id(),
            magic_program::MAGIC_CONTEXT_PUBKEY,
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

        let res = verify::fetch_and_verify_commit_result_from_logs(&ctx, *sig);
        assert_two_committees_were_committed(&ctx, &res, true);
        assert_two_committees_synchronized_count(&ctx, &res, 1);
    });
}
