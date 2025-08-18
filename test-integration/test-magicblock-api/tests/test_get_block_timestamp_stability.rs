use integration_test_tools::IntegrationTestContext;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use solana_transaction_status::UiTransactionEncoding;

#[test]
fn test_get_block_timestamp_stability() {
    let ctx = IntegrationTestContext::try_new_ephem_only().unwrap();

    // Send a transaction to the validator
    let pubkey = Keypair::new().pubkey();
    let signature = ctx.airdrop_ephem(&pubkey, LAMPORTS_PER_SOL).unwrap();

    // Wait some time for the slot to be finalized
    ctx.wait_for_delta_slot_ephem(3).unwrap();

    let tx = ctx
        .try_ephem_client()
        .unwrap()
        .get_transaction(&signature, UiTransactionEncoding::Base64)
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
