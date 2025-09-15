use integration_test_tools::IntegrationTestContext;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use test_kit::init_logger;

fn random_pubkey() -> Pubkey {
    Keypair::new().pubkey()
}

#[test]
fn test_get_multiple_accounts_non_existing() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let pubkeys = [random_pubkey(), random_pubkey(), random_pubkey()];
    let accs = ctx.fetch_ephem_multiple_accounts(&pubkeys);
    assert!(accs.is_ok());
    assert!(accs.unwrap().iter().all(|acc| acc.is_none()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_get_multiple_accounts_both_existing_and_not() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let normal = random_pubkey();
    let missing = random_pubkey();
    let escrowed_kp = Keypair::new();

    // 1. Create iniital account with 2 SOL
    ctx.airdrop_chain(&normal, 2 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to normal on-chain account");
    let (
        _airdrop_sig,
        _escrow_sig,
        ephemeral_balance_pda,
        _deleg_record,
        escrow_lamports,
    ) = ctx
        .airdrop_chain_escrowed(&escrowed_kp, 2 * LAMPORTS_PER_SOL)
        .await
        .expect("failed to airdrop to escrowed on-chain account");

    let pubkeys =
        [normal, missing, escrowed_kp.pubkey(), ephemeral_balance_pda];
    let accs = ctx.fetch_ephem_multiple_accounts(&pubkeys);
    assert!(accs.is_ok());
    let accs = accs.unwrap();
    assert_eq!(accs.len(), 4);
    assert!(accs[0].is_some());
    assert!(accs[1].is_none());
    assert!(accs[2].is_some());
    assert!(accs[3].is_some());

    assert_eq!(accs[3].as_ref().unwrap().lamports, escrow_lamports);
}
