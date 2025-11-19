use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, system_instruction,
};
use test_kit::init_logger;

use crate::utils::init_and_delegate_flexi_counter;
mod utils;

fn log_accounts_balances(
    ctx: &IntegrationTestContext,
    stage: &str,
    counter: &Pubkey,
    payer: &Pubkey,
    escrow: &Pubkey,
) -> (u64, u64, u64) {
    let accs = ctx
        .fetch_ephem_multiple_accounts(&[*counter, *payer, *escrow])
        .unwrap();
    let [counter_acc, payer_acc, escrow_acc] = accs.as_slice() else {
        panic!("Expected 3 accounts, got {:#?}", accs);
    };

    let counter_balance =
        counter_acc.as_ref().unwrap().lamports as f64 / LAMPORTS_PER_SOL as f64;
    let payer_balance =
        payer_acc.as_ref().unwrap().lamports as f64 / LAMPORTS_PER_SOL as f64;
    let escrow_balance =
        escrow_acc.as_ref().unwrap().lamports as f64 / LAMPORTS_PER_SOL as f64;
    debug!("--- {stage} ---");
    debug!("Counter {counter}: {counter_balance} SOL");
    debug!("Payer   {payer}: {payer_balance} SOL");
    debug!("Escrow  {escrow} {escrow_balance} SOL");

    (
        counter_acc.as_ref().unwrap().lamports,
        payer_acc.as_ref().unwrap().lamports,
        escrow_acc.as_ref().unwrap().lamports,
    )
}

#[ignore = "We are still evaluating escrow functionality that allows anything except just paying fees"]
#[test]
fn test_transfer_from_escrow_to_delegated_account() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    // 1. Create account with 2 SOL + escrow 1 SOL and a counter account
    let kp_counter = Keypair::new();
    let kp_escrowed = Keypair::new();

    let counter_pda = init_and_delegate_flexi_counter(&ctx, &kp_counter);
    let (
        _airdrop_sig,
        _escrow_sig,
        ephemeral_balance_pda,
        _deleg_record,
        escrow_lamports,
    ) = ctx
        .airdrop_chain_escrowed(&kp_escrowed, 2 * LAMPORTS_PER_SOL)
        .unwrap();

    let (_, _, ephem_escrow_lamports) = log_accounts_balances(
        &ctx,
        "After delegation and escrowed airdrop",
        &counter_pda,
        &kp_escrowed.pubkey(),
        &ephemeral_balance_pda,
    );
    assert_eq!(ephem_escrow_lamports, escrow_lamports);

    // 2. Transfer 0.5 SOL from kp1 to counter pda
    let transfer_amount = LAMPORTS_PER_SOL / 2;
    let transfer_ix = system_instruction::transfer(
        &kp_escrowed.pubkey(),
        &counter_pda,
        transfer_amount,
    );
    let (sig, confirmed) = ctx
        .send_and_confirm_instructions_with_payer_ephem(
            &[transfer_ix],
            &kp_escrowed,
        )
        .unwrap();

    debug!("Transfer tx sig: {sig} ({confirmed}) ");

    // 3. Check balances
    let (counter_balance, _, escrow_balance) = log_accounts_balances(
        &ctx,
        "After transfer from escrow to counter",
        &counter_pda,
        &kp_escrowed.pubkey(),
        &ephemeral_balance_pda,
    );
    let escrow_balance = escrow_balance as f64 / LAMPORTS_PER_SOL as f64;
    let counter_balance = counter_balance as f64 / LAMPORTS_PER_SOL as f64;

    // Received 1 SOL then transferred 0.5 SOL + tx fee
    assert!((0.4..=0.5).contains(&escrow_balance));
    // Airdropped 2 SOL - escrowed half
    assert!(escrow_balance >= 1.0);
    // Received 0.5 SOL
    assert!((0.5..0.6).contains(&counter_balance));
}
