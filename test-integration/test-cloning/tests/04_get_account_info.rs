use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use test_kit::init_logger;

fn random_pubkey() -> Pubkey {
    Keypair::new().pubkey()
}

fn oncurve_keypair() -> Keypair {
    let mut kp = Keypair::new();
    while !kp.pubkey().is_on_curve() {
        kp = Keypair::new();
    }
    kp
}

#[test]
fn test_get_account_info_non_existing() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let pubkey = random_pubkey();
    let acc = ctx.fetch_ephem_account(pubkey);
    assert!(acc.is_err());
}

#[test]
fn test_get_account_info_existing_not_delegated() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    // 1. Create iniital account with 2 SOL
    let pubkey = random_pubkey();
    let sig = ctx
        .airdrop_chain(&pubkey, 2 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to on-chain account");

    debug!("Airdrop 1 tx: {sig}");

    // 2. Get it the first time
    let acc = ctx.fetch_ephem_account(pubkey);
    debug!("Account: {acc:#?}");
    assert!(acc.is_ok());
    assert_eq!(acc.unwrap().lamports, 2 * LAMPORTS_PER_SOL);

    // 3. Add one SOL
    let sig = ctx
        .airdrop_chain(&pubkey, LAMPORTS_PER_SOL)
        .expect("failed to airdrop to on-chain account");
    debug!("Airdrop 2 tx: {sig}");

    // 4. Get it the second time
    let acc = ctx.fetch_ephem_account(pubkey);
    debug!("Account: {acc:#?}");
    assert!(acc.is_ok());
    assert_eq!(acc.unwrap().lamports, 3 * LAMPORTS_PER_SOL);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_get_account_info_escrowed() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    // 1. Create account with 4 SOL + escrow 2 SOL
    let kp = oncurve_keypair();
    let (
        airdrop_sig,
        escrow_sig,
        ephemeral_balance_pda,
        _deleg_record,
        escrow_lamports,
    ) = ctx
        .airdrop_chain_escrowed(&kp, 4 * LAMPORTS_PER_SOL)
        .await
        .unwrap();
    debug!("Airdrop + escrow tx: {airdrop_sig}, {escrow_sig}");

    // 2. It should now contain the account itself and the escrow
    let acc = ctx.fetch_ephem_account(kp.pubkey());
    let escrow_acc = ctx.fetch_ephem_account(ephemeral_balance_pda);
    debug!("Account: {acc:#?}");
    debug!("Escrow Account: {escrow_acc:#?}");
    assert!(acc.is_ok());
    assert!(escrow_acc.is_ok());
    assert_eq!(escrow_acc.unwrap().lamports, escrow_lamports);
}
