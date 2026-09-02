use magicblock_chainlink::{
    AccountFetchEntrypoint,
    testing::{
        context::TestContext,
        deleg::add_delegation_record_for,
        eatas::{
            EATA_PROGRAM_ID, create_ata_account, create_eata_account,
            derive_ata, derive_eata,
        },
        init_logger,
    },
};
use solana_account::{AccountMode, AccountSharedData, ReadableAccount};
use solana_keypair::Keypair;
use solana_program::{program_option::COption, program_pack::Pack};
use solana_pubkey::{Pubkey, pubkey};
use solana_signer::Signer;
use spl_token::state::AccountState;
use tracing::debug;

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

    ctx.rpc_client.add_account(ata_pubkey, ata.clone());
    ctx.rpc_client.add_account(eata_pubkey, eata.clone());

    // Add delegation record for ATA delegated to our validator
    let validator = ctx.validator_pubkey;
    add_delegation_record_for(
        &ctx.rpc_client,
        eata_pubkey,
        validator,
        EATA_PROGRAM_ID,
    );

    // Ensure account (this triggers fetch_cloner logic including ATA/eATA handling)
    let pubkeys = [ata_pubkey];
    ctx.chainlink
        .ensure_accounts(&pubkeys, AccountFetchEntrypoint::RpcGetAccount)
        .await
        .expect("ensure_accounts ok");
    debug!("res: {:?}", ());

    // Cloned account should match eATA data (replacement)
    let reader = |account: &AccountSharedData| {
        (
            spl_token::state::Account::unpack_from_slice(account.data()),
            account.is(AccountMode::Delegated),
        )
    };
    let cloned = ctx
        .bank
        .accounts()
        .loader()
        .read(&ata_pubkey, reader)
        .unwrap()
        .expect("ATA should be cloned into bank");
    let (spl_token_account, delegated) = cloned;
    let spl_token_account = spl_token_account.unwrap();
    assert_eq!(spl_token_account.mint, mint);
    assert_eq!(spl_token_account.amount, amount);
    assert_eq!(spl_token_account.owner, wallet_owner);
    assert_eq!(
        spl_token_account.close_authority,
        COption::Some(Pubkey::default())
    );
    assert_eq!(spl_token_account.state, AccountState::Initialized);
    assert_eq!(spl_token_account.delegated_amount, 0);
    assert!(spl_token_account.is_native.is_none());
    assert!(delegated)
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

    ctx.rpc_client.add_account(ata_pubkey, ata.clone());

    // Note: No delegation record added here
    let pubkeys = [ata_pubkey];
    ctx.chainlink
        .ensure_accounts(&pubkeys, AccountFetchEntrypoint::RpcGetAccount)
        .await
        .expect("ensure_accounts ok");

    let cloned = ctx
        .bank
        .accounts()
        .loader()
        .read(&ata_pubkey, |account| {
            (account.data().to_vec(), account.is(AccountMode::Delegated))
        })
        .unwrap()
        .expect("ATA should be cloned");

    // Should keep original ATA data since not delegated
    assert_eq!(cloned.0, ata.data());
    assert!(!cloned.1)
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

    ctx.rpc_client.add_account(ata_pubkey, ata.clone());
    ctx.rpc_client.add_account(eata_pubkey, eata.clone());

    // Add delegation record to a random validator
    add_delegation_record_for(
        &ctx.rpc_client,
        eata_pubkey,
        Keypair::new().pubkey(),
        wallet_owner,
    );

    // Ensure account (this triggers fetch_cloner logic including ATA/eATA handling)
    let pubkeys = [ata_pubkey];
    ctx.chainlink
        .ensure_accounts(&pubkeys, AccountFetchEntrypoint::RpcGetAccount)
        .await
        .expect("ensure_accounts ok");
    debug!("res: {:?}", ());

    // Cloned account should still be the ata, since the eata is not delegated to our validator
    let cloned = ctx
        .bank
        .accounts()
        .loader()
        .read(&ata_pubkey, |account| {
            (account.data().to_vec(), account.is(AccountMode::Delegated))
        })
        .unwrap()
        .expect("ATA should be cloned into bank");

    // Should keep original ATA data since not delegated to us
    assert_eq!(cloned.0, ata.data());
    assert!(!cloned.1)
}
