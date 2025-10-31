use assert_matches::assert_matches;
use dlp::pda::delegation_record_pda_from_delegated_account;
use log::*;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_cloned, assert_not_found, assert_not_subscribed,
    assert_remain_undelegating, assert_subscribed_without_delegation_record,
    testing::deleg::add_delegation_record_for,
};
use solana_account::{Account, AccountSharedData};
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;
use utils::test_context::TestContext;

mod utils;

use magicblock_chainlink::testing::init_logger;

use crate::utils::accounts::compressed_account_shared_with_owner_and_slot;
const CURRENT_SLOT: u64 = 11;

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

// NOTE: Case comments refer to the case studies in the relevant tabs of draw.io document, i.e. Fetch

// -----------------
// Account does not exist
// -----------------
#[tokio::test]
async fn test_write_non_existing_account() {
    let TestContext {
        chainlink, cloner, ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    assert_not_found!(res, &pubkeys);
    assert_not_cloned!(cloner, &pubkeys);
    assert_not_subscribed!(chainlink, &[&pubkey]);
}

// -----------------
// BasicScenarios: Case 1 Account is initialized and never delegated
// -----------------
#[tokio::test]
async fn test_existing_account_undelegated() {
    let TestContext {
        chainlink,
        rpc_client,
        cloner,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    rpc_client.add_account(pubkey, Account::default());

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    assert_cloned_as_undelegated!(cloner, &pubkeys, CURRENT_SLOT);
    assert_subscribed_without_delegation_record!(chainlink, &[&pubkey]);
}

// -----------------
// Failure cases account with missing/invalid delegation record
// -----------------
#[tokio::test]
async fn test_existing_account_missing_delegation_record() {
    let TestContext {
        chainlink,
        rpc_client,
        cloner,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    rpc_client.add_account(
        pubkey,
        Account {
            owner: dlp::id(),
            ..Default::default()
        },
    );

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    assert_cloned_as_undelegated!(cloner, &pubkeys, CURRENT_SLOT);
    assert_subscribed_without_delegation_record!(chainlink, &[&pubkey]);
}

// -----------------
// BasicScenarios:Case 2 Account is initialized and already delegated to us
// -----------------
#[tokio::test]
async fn test_write_existing_account_valid_delegation_record() {
    let TestContext {
        chainlink,
        rpc_client,
        validator_pubkey,
        cloner,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();

    let acc = Account {
        owner: dlp::id(),
        lamports: 1_234,
        ..Default::default()
    };
    rpc_client.add_account(pubkey, acc);

    let deleg_record_pubkey =
        add_delegation_record_for(&rpc_client, pubkey, validator_pubkey, owner);

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    // The account is cloned into the bank as delegated, the delegation record isn't
    assert_cloned_as_delegated!(cloner, &[pubkey], CURRENT_SLOT, owner);
    assert_not_cloned!(cloner, &[deleg_record_pubkey]);

    assert_not_subscribed!(
        chainlink,
        &[&deleg_record_pubkey, &validator_pubkey]
    );
}

// -----------------
// BasicScenarios:Case 3: Account Initialized and Already Delegated to Other
// -----------------
#[tokio::test]
async fn test_write_existing_account_other_authority() {
    let TestContext {
        chainlink,
        rpc_client,
        cloner,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    let account = Account {
        owner: dlp::id(),
        ..Default::default()
    };
    rpc_client.add_account(pubkey, account);

    let owner = Pubkey::new_unique();
    let authority = Pubkey::new_unique();
    let deleg_record_pubkey =
        add_delegation_record_for(&rpc_client, pubkey, authority, owner);

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    // The account is cloned into the bank as undelegated, the delegation record isn't
    assert_cloned_as_undelegated!(cloner, &pubkeys, CURRENT_SLOT, owner);
    assert_not_cloned!(cloner, &[deleg_record_pubkey]);

    assert_subscribed_without_delegation_record!(chainlink, &[&pubkey]);
}

// -----------------
// Account is in the process of being undelegated and its owner is the delegation program
// -----------------
#[tokio::test]
async fn test_write_account_being_undelegated() {
    let TestContext {
        chainlink,
        rpc_client,
        bank,
        cloner,
        ..
    } = setup(CURRENT_SLOT).await;

    let authority = Pubkey::new_unique();
    let pubkey = Pubkey::new_unique();

    // The account is still delegated to us on chain
    let account = Account {
        owner: dlp::id(),
        ..Default::default()
    };
    let owner = Pubkey::new_unique();
    rpc_client.add_account(pubkey, account);

    add_delegation_record_for(&rpc_client, pubkey, authority, owner);

    // The same account is already marked as undelegated in the bank
    // (setting the owner to the delegation program marks it as _undelegating_)
    let mut shared_data = AccountSharedData::from(Account {
        owner: dlp::id(),
        data: vec![0; 100],
        ..Default::default()
    });
    shared_data.set_remote_slot(CURRENT_SLOT);
    bank.insert(pubkey, shared_data);

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");
    assert_remain_undelegating!(cloner, &pubkeys, CURRENT_SLOT);
}

// -----------------
// Invalid Cases
// -----------------
#[tokio::test]
async fn test_write_existing_account_invalid_delegation_record() {
    let TestContext {
        chainlink,
        rpc_client,
        cloner,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    rpc_client.add_account(
        pubkey,
        Account {
            owner: dlp::id(),
            ..Default::default()
        },
    );
    let deleg_record_pubkey =
        delegation_record_pda_from_delegated_account(&pubkey);
    rpc_client.add_account(
        deleg_record_pubkey,
        Account {
            owner: dlp::id(),
            data: vec![1, 2, 3],
            ..Default::default()
        },
    );

    let res = chainlink.ensure_accounts(&[pubkey], None).await;
    debug!("res: {res:?}");

    assert_matches!(res, Err(_));
    assert!(cloner.get_account(&pubkey).is_none());

    assert_not_subscribed!(chainlink, &[&deleg_record_pubkey, &pubkey]);
}

// -----------------
// Compressed delegation record is initialized and delegated to us
// -----------------
#[tokio::test]
async fn test_compressed_delegation_record_delegated() {
    let TestContext {
        chainlink,
        photon_client,
        cloner,
        validator_pubkey,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let compressed_account = compressed_account_shared_with_owner_and_slot(
        pubkey,
        validator_pubkey,
        owner,
        CURRENT_SLOT,
    );
    photon_client.add_account(
        pubkey,
        compressed_account.clone().into(),
        CURRENT_SLOT,
    );

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    assert_cloned_as_delegated!(cloner, &pubkeys, CURRENT_SLOT);
    assert_not_subscribed!(chainlink, &[&pubkey]);
}

// -----------------
// Compressed delegation record is initialized and delegated to another authority
// -----------------
#[tokio::test]
async fn test_compressed_delegation_record_delegated_to_other() {
    let TestContext {
        chainlink,
        photon_client,
        cloner,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    let authority = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let compressed_account = compressed_account_shared_with_owner_and_slot(
        pubkey,
        authority,
        owner,
        CURRENT_SLOT,
    );
    photon_client.add_account(
        pubkey,
        compressed_account.clone().into(),
        CURRENT_SLOT,
    );

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    assert_cloned_as_undelegated!(cloner, &pubkeys, CURRENT_SLOT);
    assert_subscribed_without_delegation_record!(chainlink, &[&pubkey]);
}

// -----------------
// Compressed delegation record is initialized and delegated to us and the pda exists
// -----------------
#[tokio::test]
async fn test_compressed_delegation_record_delegated_shadows_pda() {
    let TestContext {
        chainlink,
        rpc_client,
        photon_client,
        cloner,
        validator_pubkey,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let compressed_account = compressed_account_shared_with_owner_and_slot(
        pubkey,
        validator_pubkey,
        owner,
        CURRENT_SLOT,
    );
    photon_client.add_account(
        pubkey,
        compressed_account.clone().into(),
        CURRENT_SLOT,
    );
    rpc_client.add_account(pubkey, Account::default());

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    assert_cloned_as_delegated!(cloner, &pubkeys, CURRENT_SLOT);
    assert_not_subscribed!(chainlink, &[&pubkey]);
}

// -----------------
// Compressed delegation record is initialized and empty (undelegated)
// -----------------
#[tokio::test]
async fn test_compressed_account_undelegated() {
    let TestContext {
        chainlink,
        photon_client,
        cloner,
        ..
    } = setup(CURRENT_SLOT).await;

    let pubkey = Pubkey::new_unique();
    photon_client.add_account(pubkey, Account::default(), CURRENT_SLOT);

    let pubkeys = [pubkey];
    let res = chainlink.ensure_accounts(&pubkeys, None).await.unwrap();
    debug!("res: {res:?}");

    assert_cloned_as_undelegated!(cloner, &pubkeys, CURRENT_SLOT);
    assert_subscribed_without_delegation_record!(chainlink, &[&pubkey]);
}
