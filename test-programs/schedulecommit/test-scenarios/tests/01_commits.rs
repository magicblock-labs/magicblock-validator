use log::*;
use std::str::FromStr;

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
    get_context_with_delegated_committees, iteration_count,
};
mod utils;

#[test]
fn test_committing_two_accounts() {
    init_logger!();
    info!("==== test_committing_two_accounts ====");

    for i in 0..iteration_count() {
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
            // Work around the different solana_sdk versions by creating pubkey from str
            Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
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
        info!("{}: {} '{:?}'", i, sig, tx_res);

        let res = verify::fetch_commit_result_from_logs(&ctx, *sig);
        assert_two_committees_were_committed(&ctx, &res);
        assert_two_committees_synchronized_count(&ctx, &res, 1);
    }
}
