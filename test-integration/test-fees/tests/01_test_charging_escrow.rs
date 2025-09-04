use anyhow::Context;
use integration_test_tools::IntegrationTestContext;
use log::*;
use test_tools_core::init_logger;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use program_schedulecommit::api::init_payer_escrow;
use ephemeral_rollups_sdk::pda::ephemeral_balance_pda_from_payer;
use solana_sdk::account::ReadableAccount;
use solana_sdk::commitment_config::CommitmentConfig;

#[test]
fn test_charging_escrow_with_not_delegated_feepayer() {
    init_logger!();
    info!("==== test_charging_escrow_with_not_delegated_feepayer ====");
    let ctx = IntegrationTestContext::try_new().unwrap();
    let ephem_client = ctx.try_ephem_client().unwrap();
    let chain_client = ctx.try_chain_client().unwrap();
    let ephem_blockhash = ctx.ephem_blockhash.unwrap();

    let payer = Keypair::new();

    // Init and fund the escrow for the payer
    assert!(chain_client.request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL).is_ok());
    let mut ixs = init_payer_escrow(payer.pubkey()).to_vec();
    ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(1_000_000));
    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&payer.pubkey()),
        &[&payer],
        ctx.chain_blockhash.unwrap(),
    );
    let res_escrow = chain_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            CommitmentConfig::confirmed(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .with_context(|| "Failed to escrow fund for payer");
    println!("res_escrow: {res_escrow:?}");
    assert!(res_escrow.is_ok());

    // Assert the escrow account has been cloned in the ephemeral client
    let escrow_pda = ephemeral_balance_pda_from_payer(&payer.pubkey(), 0);
    assert!(ephem_client.request_airdrop(&escrow_pda, LAMPORTS_PER_SOL).is_err());
    assert!(ephem_client.get_account(&escrow_pda).unwrap().lamports() > 0);

    // Record initial balances
    let init_payer_balance = chain_client.get_account(&payer.pubkey()).map( |a| a.lamports()).unwrap_or(0);
    let init_escrow_balance = ephem_client.get_account(&escrow_pda).unwrap().lamports();

    // Create an arbitrary transaction
    let ix = ComputeBudgetInstruction::set_compute_unit_limit(1_000_000);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        ephem_blockhash,
    );

    let _sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            ctx.commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    assert!(res.is_ok());

    // Record final balances
    let final_payer_balance = ephem_client.get_account(&payer.pubkey()).map( |a| a.lamports()).unwrap_or(0);
    let final_escrow_balance = ephem_client.get_account(&escrow_pda).unwrap().lamports();

    // Payer balance should remain the same
    assert_eq!(init_payer_balance, final_payer_balance);

    // Fee should be charged from the escrow account
    assert_eq!(init_escrow_balance, final_escrow_balance + 5000);
}
