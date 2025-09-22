use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    system_instruction,
};
use test_kit::init_logger;

use crate::utils::init_and_delegate_flexi_counter;
mod utils;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_transfer_from_escrow_to_delegated_account() {
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
        .await
        .unwrap();

    assert_eq!(
        ctx.fetch_ephem_account(ephemeral_balance_pda)
            .unwrap()
            .lamports,
        escrow_lamports
    );

    debug!("{:#?}", ctx.fetch_ephem_account(counter_pda).unwrap());

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

    debug!("Transfer tx: {sig} {confirmed}");

    // 3. Check balances
    let accs = ctx
        .fetch_ephem_multiple_accounts(&[
            kp_escrowed.pubkey(),
            ephemeral_balance_pda,
            counter_pda,
        ])
        .unwrap();
    let [escrowed, escrow, counter] = accs.as_slice() else {
        panic!("Expected 3 accounts, got {:#?}", accs);
    };

    debug!("Escrowed : '{}': {escrowed:#?}", kp_escrowed.pubkey());
    debug!("Escrow   : '{ephemeral_balance_pda}': {escrow:#?}");
    debug!("Counter  : '{counter_pda}': {counter:#?}");

    let escrowed_balance =
        escrowed.as_ref().unwrap().lamports as f64 / LAMPORTS_PER_SOL as f64;
    let escrow_balance =
        escrow.as_ref().unwrap().lamports as f64 / LAMPORTS_PER_SOL as f64;
    let counter_balance =
        counter.as_ref().unwrap().lamports as f64 / LAMPORTS_PER_SOL as f64;

    debug!(
        "\nEscrowed balance: {escrowed_balance}\nEscrow balance  : {escrow_balance}\nCounter balance : {counter_balance}"
    );
    // Received 1 SOL then transferred 0.5 SOL + tx fee
    assert!((0.4..=0.5).contains(&escrowed_balance));
    // Airdropped 2 SOL - escrowed half
    assert!(escrow_balance >= 1.0);
    // Received 0.5 SOL
    assert!((0.5..0.6).contains(&counter_balance));
}
