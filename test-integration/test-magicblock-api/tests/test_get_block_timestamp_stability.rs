use integration_test_tools::IntegrationTestContext;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    system_instruction, transaction::Transaction,
};
use solana_transaction_status::UiTransactionEncoding;

#[test]
fn test_get_block_timestamp_stability() {
    let ctx = IntegrationTestContext::try_new().unwrap();

    // Send a transaction to the validator
    let payer_chain = Keypair::new();
    let from_keypair = Keypair::new();
    let to_keypair = Keypair::new();
    // Fund payer on chain which will fund accounts we delegate
    ctx.airdrop_chain(&payer_chain.pubkey(), 5 * LAMPORTS_PER_SOL)
        .unwrap();
    ctx.airdrop_chain_and_delegate(
        &payer_chain,
        &from_keypair,
        LAMPORTS_PER_SOL,
    )
    .unwrap();
    ctx.airdrop_chain_and_delegate(&payer_chain, &to_keypair, LAMPORTS_PER_SOL)
        .unwrap();
    let rpc_client = ctx.try_ephem_client().unwrap();
    let blockhash = rpc_client.get_latest_blockhash().unwrap();
    let transfer_tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &from_keypair.pubkey(),
            &to_keypair.pubkey(),
            1000000,
        )],
        Some(&from_keypair.pubkey()),
        &[&from_keypair],
        blockhash,
    );

    let signature = rpc_client
        .send_and_confirm_transaction(&transfer_tx)
        .unwrap();

    // Wait for the transaction's slot to be completed
    ctx.wait_for_delta_slot_ephem(3).unwrap();

    let tx = rpc_client
        .get_transaction(&signature, UiTransactionEncoding::Base64)
        .unwrap();

    let current_slot = tx.slot;
    let block_time = rpc_client.get_block_time(current_slot).unwrap();
    let ledger_block = rpc_client.get_block(current_slot).unwrap();

    assert_eq!(ledger_block.block_time, Some(block_time));
    assert_eq!(tx.block_time, Some(block_time));
}
