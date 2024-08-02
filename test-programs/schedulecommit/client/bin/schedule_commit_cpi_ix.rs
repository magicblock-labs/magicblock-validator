use std::str::FromStr;

use schedulecommit_client::{verify, ScheduleCommitTestContext};
use schedulecommit_program::api::schedule_commit_cpi_instruction;
use sleipnir_core::magic_program;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{pubkey::Pubkey, signer::Signer, transaction::Transaction};

pub fn main() {
    let ctx = if std::env::var("RK").is_ok() {
        ScheduleCommitTestContext::new_random_keys(2)
    } else {
        ScheduleCommitTestContext::new(2)
    };

    ctx.init_committees().unwrap();
    ctx.delegate_committees().unwrap();

    let ScheduleCommitTestContext {
        payer,
        committees,
        commitment,
        ephem_client,
        validator_identity,
        ephem_blockhash,
        ..
    } = &ctx;

    // NOTE: at this point the payer doesn't exist in the ephem yet
    // It will be cloned including it's balance at the time the
    // schedule commit is executed
    let payer_start_balance =
        ctx.fetch_chain_account_balance(payer.pubkey()).unwrap();

    let ix = schedule_commit_cpi_instruction(
        payer.pubkey(),
        *validator_identity,
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
    eprintln!("Transaction res: '{:?}'", res);

    let res = verify::fetch_commit_result_from_logs(&ctx, *sig);
    let pda1 = committees[0].1;
    let pda2 = committees[1].1;

    assert_eq!(res.included.len(), 2, "includes 2 pdas");
    assert_eq!(res.excluded.len(), 0, "excludes 0 pdas");

    let commit1 = res.included.get(&pda1);
    let commit2 = res.included.get(&pda2);
    assert!(commit1.is_some(), "should have committed pda1");
    assert!(commit2.is_some(), "should have committed pda2");

    assert_eq!(
        commit1.unwrap().ephem_account.count,
        1,
        "pda1 count is 1 on ephem"
    );
    assert_eq!(
        commit1.unwrap().chain_account.count,
        1,
        "pda1 count is 1 on chain"
    );
    assert_eq!(
        commit2.unwrap().ephem_account.count,
        1,
        "pda2 count is 1 on ephem"
    );
    assert_eq!(
        commit2.unwrap().chain_account.count,
        1,
        "pda2 count is 1 on chain"
    );

    assert_eq!(res.sigs.len(), 1, "should have 1 on chain sig");

    let payer_end_balance =
        ctx.fetch_ephem_account_balance(payer.pubkey()).unwrap();

    const TX_COST: u64 = 10_000;
    assert_eq!(
        payer_start_balance - TX_COST,
        payer_end_balance,
        "payer balance should be decremented by tx cost"
    );

    // Used to verify that test passed
    println!("Success");
}
