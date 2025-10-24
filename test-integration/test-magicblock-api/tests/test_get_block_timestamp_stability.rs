use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    system_instruction,
};
use solana_transaction_status::UiTransactionEncoding;
use test_kit::init_logger;

#[test]
fn test_get_block_timestamp_stability() {
    init_logger!();

    let ctx = IntegrationTestContext::try_new().unwrap();
    let chain_payer = Keypair::new();
    ctx.airdrop_chain(&chain_payer.pubkey(), 10 * LAMPORTS_PER_SOL)
        .unwrap();

    let from_keypair = Keypair::new();
    let to_keypair = Keypair::new();
    ctx.airdrop_chain_and_delegate(
        &chain_payer,
        &from_keypair,
        LAMPORTS_PER_SOL,
    )
    .unwrap();
    ctx.airdrop_chain_and_delegate(&chain_payer, &to_keypair, LAMPORTS_PER_SOL)
        .unwrap();
    debug!(
        "✅ Airdropped and delegated from {} and to {}",
        from_keypair.pubkey(),
        to_keypair.pubkey()
    );

    // Send a transaction to the validator
    let (sig, confirmed) = ctx
        .send_and_confirm_instructions_with_payer_ephem(
            &[system_instruction::transfer(
                &from_keypair.pubkey(),
                &to_keypair.pubkey(),
                1000000,
            )],
            &from_keypair,
        )
        .unwrap();
    debug!("✅ Transfer tx {sig} confirmed: {confirmed}");
    assert!(confirmed);

    // Wait for the transaction's slot to be completed
    ctx.wait_for_delta_slot_ephem(3).unwrap();

    let tx = ctx
        .try_ephem_client()
        .unwrap()
        .get_transaction(&sig, UiTransactionEncoding::Base64)
        .unwrap();

    let current_slot = tx.slot;
    let block_time = ctx
        .try_ephem_client()
        .unwrap()
        .get_block_time(current_slot)
        .unwrap();
    let ledger_block = ctx
        .try_ephem_client()
        .unwrap()
        .get_block(current_slot)
        .unwrap();

    assert_eq!(ledger_block.block_time, Some(block_time));
    assert_eq!(tx.block_time, Some(block_time));
}
