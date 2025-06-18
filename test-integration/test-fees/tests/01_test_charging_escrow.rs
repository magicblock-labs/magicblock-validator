use std::str::FromStr;
use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::account::ReadableAccount;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::message::Message;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::{Transaction};
use test_tools_core::init_logger;
use ephemeral_rollups_sdk::pda::{
    ephemeral_balance_pda_from_payer,
};

#[test]
fn test_charging_escrow() {
    // Frequent commits were running every time `accounts.commits.frequency_millis` expired
    // even when no accounts needed to be committed. This test checks that the bug is fixed.
    // We can remove it once we no longer commit accounts frequently.
    init_logger!();
    info!("==== test_fees_can_be_charged_from_escrow ====");

    let ctx = IntegrationTestContext::try_new().unwrap();
    let chain_client = &ctx.try_chain_client().unwrap();
    let ephem_client = &ctx.try_ephem_client().unwrap();

    // Setup: payer and escrow funds for the fee payer
    let fee_payer = Keypair::new();
    ctx.airdrop_chain(&fee_payer.pubkey(), LAMPORTS_PER_SOL).unwrap();
    ctx.escrow_lamports_for_payer(&fee_payer).unwrap();

    // Get the ephemeral balance PDA for the fee payer
    let ephemeral_balance_pda = ephemeral_balance_pda_from_payer(&fee_payer.pubkey(), 0);
    let escrow_lamports = chain_client.get_account(&ephemeral_balance_pda).expect("Escrow account should exist").lamports();

    // 1. Perform an arbitrary transaction (with the noop program) to trigger the fee collection
    let noop_program_id = Pubkey::from_str("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV").unwrap();
    let instruction = Instruction::new_with_bytes(
        noop_program_id,
        &[],
        vec![AccountMeta::new(fee_payer.pubkey(), true)],
    );
    let message = Message::new(&[instruction], None);
    let tx = Transaction::new(&[fee_payer], message, ctx.ephem_blockhash.unwrap());

    let signature = ctx
        .try_ephem_client()
        .unwrap()
        .send_and_confirm_transaction_with_spinner(&tx)
        .unwrap();
    eprintln!("Transaction executed successfully: {}", signature);

    // 2. Check escrow account was cloned in the ephemeral and fees were charged
    let escrow_acc = ephem_client.get_account(&ephemeral_balance_pda).expect("Escrow account should exist");
    assert_eq!(escrow_acc.lamports, escrow_lamports - 5000, "Escrow account should have been charged 5000 lamports for fees");
}
