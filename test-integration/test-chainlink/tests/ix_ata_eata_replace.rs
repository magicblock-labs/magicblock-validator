use log::debug;
use magicblock_chainlink::{
    testing::{deleg::add_delegation_record_for, init_logger},
    AccountFetchOrigin,
};
use solana_account::{ReadableAccount};
use solana_pubkey::{pubkey, Pubkey};
use solana_sdk::signature::{Keypair, Signer};
use spl_token::solana_program::program_pack::Pack;
use spl_token::state::AccountState;
use magicblock_chainlink::testing::eatas::{create_ata_account, create_eata_account, derive_ata, derive_eata};
use test_chainlink::test_context::TestContext;


#[tokio::test]
async fn ixtest_ata_eata_replace_when_delegated_to_us() {
    init_logger();

    // Use mocked TestContext (no external RPC)
    let slot = 100u64;
    let ctx = TestContext::init(slot).await;

    // Wallet owner and mint
    let wallet_owner = Keypair::new().pubkey();
    let mint = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    let amount = 200;

    // Derive ATA and eATA addresses
    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_pubkey = derive_eata(&wallet_owner, &mint);

    // Create mock ATA and eATA accounts
    let ata = create_ata_account(&wallet_owner, &mint);
    let eata = create_eata_account(&wallet_owner, &mint, amount, true);

    ctx.rpc_client.add_account(
        ata_pubkey,
        ata.clone(),
    );
    ctx.rpc_client.add_account(
        eata_pubkey,
        eata.clone(),
    );

    // Add delegation record for ATA delegated to our validator
    let validator = ctx.validator_pubkey;
    add_delegation_record_for(&ctx.rpc_client, eata_pubkey, validator, wallet_owner);

    // Ensure account (this triggers fetch_cloner logic including ATA/eATA handling)
    let pubkeys = [ata_pubkey];
    let res = ctx
        .chainlink
        .ensure_accounts(&pubkeys, None, AccountFetchOrigin::GetAccount, None)
        .await
        .expect("ensure_accounts ok");
    debug!("res: {:?}", res);

    // Cloned account should match eATA data (replacement)
    let cloned = ctx
        .cloner
        .get_account(&ata_pubkey)
        .expect("ATA should be cloned into bank");
    let spl_token_account = spl_token::state::Account::unpack_from_slice(cloned.data()).unwrap();
    assert_eq!(spl_token_account.mint, mint);
    assert_eq!(spl_token_account.amount, amount);
    assert_eq!(spl_token_account.owner, wallet_owner);
    assert!(spl_token_account.close_authority.is_none());
    assert_eq!(spl_token_account.state, AccountState::Initialized);
    assert_eq!(spl_token_account.delegated_amount, 0);
    assert!(spl_token_account.is_native.is_none());
    assert!(cloned.delegated())
}

#[tokio::test]
async fn ixtest_ata_eata_no_replace_when_not_delegated() {
    init_logger();

    let slot = 101u64;
    let ctx = TestContext::init(slot).await;

    let wallet_owner = Keypair::new().pubkey();
    let mint = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let ata = create_ata_account(&wallet_owner, &mint);

    ctx.rpc_client.add_account(
        ata_pubkey,
        ata.clone(),
    );

    // Note: No delegation record added here
    let pubkeys = [ata_pubkey];
    let _res = ctx
        .chainlink
        .ensure_accounts(&pubkeys, None, AccountFetchOrigin::GetAccount, None)
        .await
        .expect("ensure_accounts ok");

    let cloned = ctx
        .cloner
        .get_account(&ata_pubkey)
        .expect("ATA should be cloned");

    // Should keep original ATA data since not delegated
    assert_eq!(cloned.data(), ata.data());
    assert!(!cloned.delegated())
}

#[tokio::test]
async fn ixtest_ata_eata_no_replace_when_not_delegated_to_us() {
    init_logger();

    // Use mocked TestContext (no external RPC)
    let slot = 100u64;
    let ctx = TestContext::init(slot).await;

    // Wallet owner and mint
    let wallet_owner = Keypair::new().pubkey();
    let mint = Pubkey::new_unique();
    let amount = 200;

    // Derive ATA and eATA addresses
    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_pubkey = derive_eata(&wallet_owner, &mint);

    // Create mock ATA and eATA accounts
    let ata = create_ata_account(&wallet_owner, &mint);
    let eata = create_eata_account(&wallet_owner, &mint, amount, true);

    ctx.rpc_client.add_account(
        ata_pubkey,
        ata.clone(),
    );
    ctx.rpc_client.add_account(
        eata_pubkey,
        eata.clone(),
    );

    // Add delegation record to a random validator
    add_delegation_record_for(&ctx.rpc_client, eata_pubkey, Keypair::new().pubkey(), wallet_owner);

    // Ensure account (this triggers fetch_cloner logic including ATA/eATA handling)
    let pubkeys = [ata_pubkey];
    let res = ctx
        .chainlink
        .ensure_accounts(&pubkeys, None, AccountFetchOrigin::GetAccount, None)
        .await
        .expect("ensure_accounts ok");
    debug!("res: {:?}", res);

    // Cloned account should still be the ata, since the eata is not delegated to our validator
    let cloned = ctx
        .cloner
        .get_account(&ata_pubkey)
        .expect("ATA should be cloned into bank");

    // Should keep original ATA data since not delegated to us
    assert_eq!(cloned.data(), ata.data());
    assert!(!cloned.delegated())
}
