use integration_test_tools::run_test;
use log::*;

use integration_test_tools::conversions::pubkey_from_magic_program;
use schedulecommit_client::{verify, ScheduleCommitTestContextFields};
use schedulecommit_program::api::schedule_commit_cpi_instruction;
use sleipnir_core::magic_program;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{pubkey::Pubkey, signer::Signer, transaction::Transaction};
use test_tools_core::init_logger;
use utils::{
    assert_two_committees_synchronized_count,
    assert_two_committees_were_committed,
    get_context_with_delegated_committees,
};
mod utils;

#[test]
fn test_committing_two_accounts() {
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

        let ix = schedule_commit_cpi_instruction(
            payer.pubkey(),
            pubkey_from_magic_program(magic_program::id()),
            pubkey_from_magic_program(magic_program::MAGIC_CONTEXT_PUBKEY),
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

        let res = verify::fetch_commit_result_from_logs(&ctx, *sig);
        assert_two_committees_were_committed(&ctx, &res);
        assert_two_committees_synchronized_count(&ctx, &res, 1);
    });
}
