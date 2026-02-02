use integration_test_tools::IntegrationTestContext;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use test_kit::init_logger;
use tracing::*;

fn random_pubkey() -> Pubkey {
    Keypair::new().pubkey()
}

#[test]
fn test_account_clone_logs_not_delegated() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    // 1. Create account not delegated
    let pubkey = random_pubkey();
    let airdrop_sig = ctx
        .airdrop_chain(&pubkey, 2 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop");
    debug!("Airdrop tx: {airdrop_sig}");

    // 2. Get account from ephemeral to trigger clone
    let acc = ctx.fetch_ephem_account(pubkey);
    assert!(acc.is_ok());

    // 3. Find the cloning transaction
    let all_sigs = ctx
        .get_signaturestats_for_address_ephem(&pubkey)
        .expect("failed to get transaction signatures");
    debug!("All transactions for account: {:#?}", all_sigs);

    let clone_sig = ctx
        .last_transaction_mentioning_account_ephem(&pubkey)
        .expect("failed to find cloning transaction");
    debug!("Cloning transaction (latest): {clone_sig}");

    // 4. Verify logs contain the delegation message
    ctx.assert_ephemeral_logs_contain(
        clone_sig,
        "MutateAccounts: account is not delegated to any validator",
    );
}

#[test]
fn test_account_clone_logs_delegated_to_other_validator() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    // 1. Create account and delegate to another validator
    let payer_chain = Keypair::new();
    let account_kp = Keypair::new();
    let other_validator = random_pubkey();

    ctx.airdrop_chain(&payer_chain.pubkey(), 5 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to payer");
    ctx.airdrop_chain(&account_kp.pubkey(), 2 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop account");

    let (delegate_sig, confirmed) = ctx
        .delegate_account_to_validator(
            &payer_chain,
            &account_kp,
            Some(other_validator),
        )
        .expect("failed to delegate account");
    assert!(confirmed, "Failed to confirm delegation");
    debug!("Delegation tx: {delegate_sig}");

    // 2. Get account from ephemeral to trigger clone
    let acc = ctx.fetch_ephem_account(account_kp.pubkey());
    assert!(acc.is_ok());

    // 3. Find the cloning transaction
    let clone_sig = ctx
        .last_transaction_mentioning_account_ephem(&account_kp.pubkey())
        .expect("failed to find cloning transaction");
    debug!("Cloning transaction: {clone_sig}");

    // 4. Verify logs contain the delegation message
    ctx.assert_ephemeral_logs_contain(
        clone_sig,
        &format!(
            "MutateAccounts: account is delegated to another validator: {}",
            other_validator
        ),
    );
}

#[test]
fn test_account_clone_logs_delegated_to_us() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let payer_chain = Keypair::new();
    let delegated_kp = Keypair::new();

    ctx.airdrop_chain(&payer_chain.pubkey(), 5 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to payer");

    let (airdrop_sig, deleg_sig) = ctx
        .airdrop_chain_and_delegate(
            &payer_chain,
            &delegated_kp,
            2 * LAMPORTS_PER_SOL,
        )
        .expect("failed to airdrop and delegate");
    debug!("Airdrop + delegation tx: {airdrop_sig}, {deleg_sig}");

    // 2. Get account from ephemeral to trigger clone
    let acc = ctx.fetch_ephem_account(delegated_kp.pubkey());
    assert!(acc.is_ok());

    // 3. Find the cloning transaction (should be most recent one mentioning account)
    let clone_sig = ctx
        .last_transaction_mentioning_account_ephem(&delegated_kp.pubkey())
        .expect("failed to find cloning transaction");
    debug!("Cloning transaction: {clone_sig}");

    // 4. Verify logs contain the delegated true message
    //    In that case on additional log is printed
    ctx.assert_ephemeral_logs_contain(
        clone_sig,
        "MutateAccounts: setting delegated to true",
    );
}

#[test]
fn test_account_clone_logs_confined_delegation() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    // 1. Create account and delegate to system program (confined)
    let payer_chain = Keypair::new();
    let account_kp = Keypair::new();

    ctx.airdrop_chain(&payer_chain.pubkey(), 5 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to payer");
    ctx.airdrop_chain(&account_kp.pubkey(), 2 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop account");

    let (delegate_sig, confirmed) = ctx
        .delegate_account_to_validator(
            &payer_chain,
            &account_kp,
            Some(solana_sdk::system_program::id()),
        )
        .expect("failed to delegate to system program");
    assert!(confirmed, "Failed to confirm delegation");
    debug!("Delegation tx: {delegate_sig}");

    // 2. Get account from ephemeral to trigger clone
    let acc = ctx.fetch_ephem_account(account_kp.pubkey());
    assert!(acc.is_ok());

    // 3. Find the cloning transaction
    let clone_sig = ctx
        .last_transaction_mentioning_account_ephem(&account_kp.pubkey())
        .expect("failed to find cloning transaction");
    debug!("Cloning transaction: {clone_sig}");

    ctx.assert_ephemeral_logs_contain(
        clone_sig,
        "MutateAccounts: setting delegated to true",
    );
    ctx.assert_ephemeral_logs_contain(
        clone_sig,
        "MutateAccounts: setting confined to true",
    );
}
