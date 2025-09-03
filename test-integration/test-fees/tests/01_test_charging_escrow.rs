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
    let ixs = init_payer_escrow(payer.pubkey());
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
    assert!(res_escrow.is_ok());

    // Assert the escrow account has been cloned in the ephemeral client
    let escrow_pda = ephemeral_balance_pda_from_payer(&payer.pubkey(), 0);
    assert!(ephem_client.request_airdrop(&escrow_pda, LAMPORTS_PER_SOL).is_err());
    assert_eq!(escrow_pda.to_string(), "");
    assert!(ephem_client.get_account(&escrow_pda).is_ok());

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
    println!("res: {res:?}");
    assert_eq!(escrow_pda.to_string(), "");
    assert_eq!(payer.pubkey().to_string(), "");
    assert!(res.is_ok())
}
