use std::str::FromStr;
use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::{SanitizedTransaction, Transaction};
use test_tools_core::init_logger;

#[test]
fn test_charging_escrow() {
    // Frequent commits were running every time `accounts.commits.frequency_millis` expired
    // even when no accounts needed to be committed. This test checks that the bug is fixed.
    // We can remove it once we no longer commit accounts frequently.
    init_logger!();
    info!("==== test_fees_can_be_charged_from_escrow ====");

    let ctx = IntegrationTestContext::try_new().unwrap();
    let chain_client = &ctx.try_chain_client().unwrap();

    // 1. Perform an arbitrary transaction
    let fee_payer = Keypair::new();

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

    // 2. Check escrow account was cloned and fees were charged
    let escrow_acc = chain_client.get_account(&ephemeral_balance_pda).unwrap();

}
