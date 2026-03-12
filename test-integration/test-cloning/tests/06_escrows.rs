use integration_test_tools::{dlp_interface, IntegrationTestContext};
use solana_sdk::{
    account::Account, native_token::LAMPORTS_PER_SOL, pubkey::Pubkey,
    signature::Keypair, signer::Signer, system_instruction,
};
use test_kit::init_logger;
use tracing::*;

fn get_escrow_pda_ephem(
    ctx: &IntegrationTestContext,
    owner: &Keypair,
) -> (Pubkey, Option<Account>) {
    let (escrow_pda, _) = dlp_interface::escrow_pdas(&owner.pubkey());
    // This returns an account not found error if the account does not exist
    let acc = ctx.fetch_ephem_account(escrow_pda).ok();
    (escrow_pda, acc)
}

#[test]
fn test_cloning_escrowed_payer() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let payer_chain = Keypair::new();
    let escrowed_kp = Keypair::new();
    let delegated_kp = Keypair::new();

    ctx.airdrop_chain(&payer_chain.pubkey(), 5 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to payer_chain account");
    ctx.airdrop_chain_escrowed(&escrowed_kp, 2 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to escrowed on-chain account");

    // NOTE: the escrow is cloned from chain when we get it the first time from the ephem
    let (escrow_pda, initial_acc) = get_escrow_pda_ephem(&ctx, &escrowed_kp);
    debug!(
        "escrow account initially {}: {:#?}",
        escrow_pda, initial_acc
    );
    assert!(initial_acc.is_some());

    let ix = system_instruction::transfer(
        &escrowed_kp.pubkey(),
        &delegated_kp.pubkey(),
        LAMPORTS_PER_SOL / 2,
    );
    let (_sig, _found) = ctx
        .send_and_confirm_instructions_with_payer_ephem(&[ix], &escrowed_kp)
        .unwrap();

    // When it completes we should see an unchanged escrow inside the validator
    let (escrow_pda, after_tx_acc) = get_escrow_pda_ephem(&ctx, &escrowed_kp);
    debug!(
        "escrow account after tx {}: {:#?}",
        escrow_pda, after_tx_acc
    );
    assert_eq!(after_tx_acc, initial_acc);

    // If we then change the escrow on chain, i.e. due to another topup it will not
    // update in the ephem since it is delegated
    ctx.airdrop_chain(&escrow_pda, LAMPORTS_PER_SOL).unwrap();
    let (escrow_pda, acc) = get_escrow_pda_ephem(&ctx, &escrowed_kp);
    debug!(
        "escrow account after chain airdrop {}: {:#?}",
        escrow_pda, acc
    );
    assert!(acc.is_some());
    let acc = acc.unwrap();
    assert_eq!(acc.lamports, after_tx_acc.unwrap().lamports);
}
