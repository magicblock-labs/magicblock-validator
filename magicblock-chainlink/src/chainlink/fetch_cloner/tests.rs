use std::{collections::HashMap, sync::Arc};

use solana_account::{Account, AccountSharedData, WritableAccount};
use solana_keypair::Keypair;
use solana_sdk_ids::system_program;
use solana_signer::Signer;
use tokio::sync::mpsc;

use super::*;
use crate::{
    accounts_bank::mock::AccountsBankStub,
    assert_not_cloned, assert_not_subscribed, assert_subscribed,
    assert_subscribed_without_delegation_record,
    remote_account_provider::{
        chain_pubsub_client::mock::ChainPubsubClientMock,
        chain_slot::ChainSlot, RemoteAccountProvider,
    },
    testing::{
        accounts::{
            account_shared_with_owner, delegated_account_shared_with_owner,
            delegated_account_shared_with_owner_and_slot,
        },
        cloner_stub::ClonerStub,
        deleg::{
            add_delegation_record_for, add_delegation_record_with_actions_for,
            add_invalid_delegation_record_for,
        },
        eatas::{
            create_ata_account, create_eata_account, derive_ata, derive_eata,
            EATA_PROGRAM_ID,
        },
        init_logger,
        rpc_client_mock::{ChainRpcClientMock, ChainRpcClientMockBuilder},
        utils::{create_test_lru_cache, random_pubkey},
    },
};

type TestFetchClonerResult = (
    Arc<
        FetchCloner<
            ChainRpcClientMock,
            ChainPubsubClientMock,
            AccountsBankStub,
            ClonerStub,
        >,
    >,
    mpsc::Sender<ForwardedSubscriptionUpdate>,
);

macro_rules! _cloned_account {
    ($bank:expr,
     $account_pubkey:expr,
     $expected_account:expr,
     $expected_slot:expr,
     $delegated:expr,
     $owner:expr) => {{
        let cloned_account = $bank.get_account(&$account_pubkey);
        assert!(cloned_account.is_some());
        let cloned_account = cloned_account.unwrap();
        let mut expected_account = AccountSharedData::from($expected_account);
        expected_account.set_remote_slot($expected_slot);
        expected_account.set_delegated($delegated);
        expected_account.set_owner($owner);

        assert_eq!(cloned_account, expected_account);
        assert_eq!(cloned_account.remote_slot(), $expected_slot);
        cloned_account
    }};
}

macro_rules! assert_cloned_delegated_account {
    ($bank:expr, $account_pubkey:expr, $expected_account:expr, $expected_slot:expr, $owner:expr) => {{
        _cloned_account!(
            $bank,
            $account_pubkey,
            $expected_account,
            $expected_slot,
            true,
            $owner
        )
    }};
}

macro_rules! assert_cloned_undelegated_account {
    ($bank:expr, $account_pubkey:expr, $expected_account:expr, $expected_slot:expr, $owner:expr) => {{
        _cloned_account!(
            $bank,
            $account_pubkey,
            $expected_account,
            $expected_slot,
            false,
            $owner
        )
    }};
}

struct FetcherTestCtx {
    remote_account_provider:
        Arc<RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>>,
    accounts_bank: Arc<AccountsBankStub>,
    rpc_client: crate::testing::rpc_client_mock::ChainRpcClientMock,
    #[allow(unused)]
    forward_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
    fetch_cloner: Arc<
        FetchCloner<
            ChainRpcClientMock,
            ChainPubsubClientMock,
            AccountsBankStub,
            ClonerStub,
        >,
    >,
    #[allow(unused)]
    subscription_tx: mpsc::Sender<ForwardedSubscriptionUpdate>,
}

async fn setup<I>(
    accounts: I,
    current_slot: u64,
    validator_keypair: Keypair,
) -> FetcherTestCtx
where
    I: IntoIterator<Item = (Pubkey, Account)>,
{
    init_logger();

    let faucet_pubkey = Pubkey::new_unique();

    // Setup mock RPC client with the accounts and clock sysvar
    let accounts_map: HashMap<Pubkey, Account> = accounts.into_iter().collect();
    let rpc_client = ChainRpcClientMockBuilder::new()
        .slot(current_slot)
        .clock_sysvar_for_slot(current_slot)
        .accounts(accounts_map)
        .build();

    // Setup components
    let (updates_sender, updates_receiver) = mpsc::channel(1_000);
    let pubsub_client =
        ChainPubsubClientMock::new(updates_sender, updates_receiver);
    let accounts_bank = Arc::new(AccountsBankStub::default());
    let rpc_client_clone = rpc_client.clone();

    let (forward_tx, forward_rx) = mpsc::channel(1_000);
    let (subscribed_accounts, config) = create_test_lru_cache(1000);
    let chain_slot = Arc::<AtomicU64>::default();

    let remote_account_provider = Arc::new(
        RemoteAccountProvider::new(
            rpc_client,
            pubsub_client,
            forward_tx,
            &config,
            subscribed_accounts,
            ChainSlot::new(chain_slot),
        )
        .await
        .unwrap(),
    );
    let (fetch_cloner, subscription_tx) = init_fetch_cloner(
        remote_account_provider.clone(),
        &accounts_bank,
        validator_keypair,
        faucet_pubkey,
    );

    FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client: rpc_client_clone,
        forward_rx,
        fetch_cloner,
        subscription_tx,
    }
}

fn create_non_raw_eata_owned_account(
    pubkey: Pubkey,
    data_len: usize,
) -> (Account, Pubkey, Pubkey) {
    let (wallet_owner, mint) = loop {
        let wallet_owner = random_pubkey();
        let mint = random_pubkey();
        if derive_eata(&wallet_owner, &mint) != pubkey {
            break (wallet_owner, mint);
        }
    };

    let mut data = vec![0u8; data_len.max(72)];
    data[0..32].copy_from_slice(wallet_owner.as_ref());
    data[32..64].copy_from_slice(mint.as_ref());
    data[64..72].copy_from_slice(&777u64.to_le_bytes());

    (
        Account {
            lamports: 1_000_000,
            data,
            owner: dlp_api::id(),
            executable: false,
            rent_epoch: 0,
        },
        wallet_owner,
        mint,
    )
}

/// Helper function to initialize FetchCloner for tests with subscription updates
/// Returns (FetchCloner, subscription_sender) for simulating subscription updates in tests
fn init_fetch_cloner(
    remote_account_provider: Arc<
        RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
    >,
    bank: &Arc<AccountsBankStub>,
    validator_keypair: Keypair,
    faucet_pubkey: Pubkey,
) -> TestFetchClonerResult {
    let (subscription_tx, subscription_rx) = mpsc::channel(100);
    let cloner = Arc::new(ClonerStub::new(bank.clone()));
    let fetch_cloner = FetchCloner::new(
        &remote_account_provider,
        bank,
        &cloner,
        validator_keypair,
        faucet_pubkey,
        subscription_rx,
        None,
    );
    (fetch_cloner, subscription_tx)
}

// -----------------
// Single Account Tests
// -----------------
#[tokio::test]
async fn test_fetch_and_clone_single_non_delegated_account() {
    let validator_keypair = Keypair::new();
    let account_pubkey = random_pubkey();
    let account_owner = random_pubkey();

    // Create a non-delegated account
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        100,
        validator_keypair.insecure_clone(),
    )
    .await;

    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");

    assert!(result.is_ok());
    assert_cloned_undelegated_account!(
        accounts_bank,
        account_pubkey,
        account,
        100,
        account_owner
    );
}

#[tokio::test]
async fn test_fetch_and_clone_single_non_existing_account() {
    let validator_keypair = Keypair::new();
    let non_existing_pubkey = random_pubkey();

    // Setup with no accounts (empty collection)
    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        ..
    } = setup(
        std::iter::empty::<(Pubkey, Account)>(),
        100,
        validator_keypair.insecure_clone(),
    )
    .await;

    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[non_existing_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");

    // Verify success (non-existing accounts are handled gracefully)
    assert!(result.is_ok());

    // Verify no account was cloned
    let cloned_account = accounts_bank.get_account(&non_existing_pubkey);
    assert!(cloned_account.is_none());
}

#[tokio::test]
async fn test_fetch_and_clone_single_delegated_account_with_valid_delegation_record(
) {
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_pubkey = random_pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    // Create a delegated account (owned by dlp)
    let account = Account {
        lamports: 1_234,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    // Setup with just the delegated account
    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Add delegation record
    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    // Test fetch and clone
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");

    assert!(result.is_ok());

    // Verify account was cloned with correct delegation properties
    let cloned_account = accounts_bank.get_account(&account_pubkey);
    assert!(cloned_account.is_some());
    let cloned_account = cloned_account.unwrap();

    // The cloned account should have the delegation owner and be marked as delegated
    let mut expected_account =
        delegated_account_shared_with_owner(&account, account_owner);
    expected_account.set_remote_slot(CURRENT_SLOT);
    assert_eq!(cloned_account, expected_account);

    // Assert correct remote_slot
    assert_eq!(cloned_account.remote_slot(), CURRENT_SLOT);

    // Verify delegation record was not cloned (only the delegated account is cloned)
    assert!(accounts_bank.get_account(&deleg_record_pubkey).is_none());

    // Delegated accounts to us should not be subscribed since we control them
    assert_not_subscribed!(
        remote_account_provider,
        &[&account_pubkey, &deleg_record_pubkey]
    );
}

#[tokio::test]
async fn test_fetch_and_clone_single_delegated_account_with_different_authority(
) {
    let validator_keypair = Keypair::new();
    let different_authority = random_pubkey(); // Different authority
    let account_pubkey = random_pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    // Create a delegated account (owned by dlp)
    let account = Account {
        lamports: 1_234,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    // Setup with just the delegated account
    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Add delegation record with a different authority (not our validator)
    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        different_authority,
        account_owner,
    );

    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");

    assert!(result.is_ok());

    // Verify account was cloned but NOT marked as delegated since authority is different
    let cloned_account = accounts_bank.get_account(&account_pubkey);
    assert!(cloned_account.is_some());
    let cloned_account = cloned_account.unwrap();

    // The cloned account should have the delegation owner but NOT be marked as delegated
    // since the authority doesn't match our validator
    let mut expected_account =
        account_shared_with_owner(&account, account_owner);
    expected_account.set_remote_slot(CURRENT_SLOT);
    assert_eq!(cloned_account, expected_account);

    // Specifically verify it's not marked as delegated
    assert!(!cloned_account.delegated());

    // Assert correct remote_slot
    assert_eq!(cloned_account.remote_slot(), CURRENT_SLOT);

    // Verify delegation record was not cloned (only the delegated account is cloned)
    assert!(accounts_bank.get_account(&deleg_record_pubkey).is_none());

    assert_subscribed!(remote_account_provider, &[&account_pubkey]);
    assert_not_subscribed!(remote_account_provider, &[&deleg_record_pubkey]);
}

#[tokio::test]
async fn test_fetch_and_clone_single_delegated_account_without_delegation_record_that_has_sub(
) {
    // In case the delegation record itself was subscribed to already and then we subscribe to
    // the account itself, then the subscription to the delegation record should not be removed
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_pubkey = random_pubkey();
    let account_owner = random_pubkey();

    const CURRENT_SLOT: u64 = 100;

    // Create a delegated account (owned by dlp)
    let account = Account {
        lamports: 1_234,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    // Setup with just the delegated account
    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        fetch_cloner,
        rpc_client,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Delegation record is cloned previously
    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[deleg_record_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;
    assert!(result.is_ok());

    // Verify delegation record was cloned
    assert!(accounts_bank.get_account(&deleg_record_pubkey).is_some());

    // Fetch and clone the delegated account
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    assert!(result.is_ok());

    // Verify account was cloned correctly
    let cloned_account = accounts_bank.get_account(&account_pubkey);
    assert!(cloned_account.is_some());
    let cloned_account = cloned_account.unwrap();

    let expected_account = delegated_account_shared_with_owner_and_slot(
        &account,
        account_owner,
        CURRENT_SLOT,
    );
    assert_eq!(cloned_account, expected_account);

    // Verify delegation record was not removed
    assert!(accounts_bank.get_account(&deleg_record_pubkey).is_some());

    // The subscription to the delegation record should remain
    assert_subscribed!(remote_account_provider, &[&deleg_record_pubkey]);
    // The delegated account should not be subscribed
    assert_not_subscribed!(remote_account_provider, &[&account_pubkey]);
}

// -----------------
// Multi Account Tests
// -----------------

#[tokio::test]
async fn test_fetch_and_clone_multiple_accounts_mixed_types() {
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    // Test 1: non-delegated account, delegated account, delegation record
    let non_delegated_pubkey = random_pubkey();
    let delegated_account_pubkey = random_pubkey();
    // This is a delegation record that we are actually cloning into the validator
    let delegation_record_pubkey = random_pubkey();

    let non_delegated_account = Account {
        lamports: 500_000,
        data: vec![10, 20, 30],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let delegated_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let delegation_record_account = Account {
        lamports: 2_000_000,
        data: vec![100, 101, 102],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let accounts = [
        (non_delegated_pubkey, non_delegated_account.clone()),
        (delegated_account_pubkey, delegated_account.clone()),
        (delegation_record_pubkey, delegation_record_account.clone()),
    ];

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(accounts, CURRENT_SLOT, validator_keypair.insecure_clone()).await;

    // Add delegation record for the delegated account
    add_delegation_record_for(
        &rpc_client,
        delegated_account_pubkey,
        validator_pubkey,
        account_owner,
    );

    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[
                non_delegated_pubkey,
                delegated_account_pubkey,
                delegation_record_pubkey,
            ],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");

    assert!(result.is_ok());

    assert_cloned_undelegated_account!(
        accounts_bank,
        non_delegated_pubkey,
        non_delegated_account.clone(),
        CURRENT_SLOT,
        non_delegated_account.owner
    );

    assert_cloned_delegated_account!(
        accounts_bank,
        delegated_account_pubkey,
        delegated_account.clone(),
        CURRENT_SLOT,
        account_owner
    );

    // Verify delegation record account was cloned as non-delegated
    // (it's owned by delegation program but has no delegation record itself)
    assert_cloned_undelegated_account!(
        accounts_bank,
        delegation_record_pubkey,
        delegation_record_account,
        CURRENT_SLOT,
        dlp_api::id()
    );

    assert_subscribed_without_delegation_record!(
        remote_account_provider,
        &[&non_delegated_pubkey, &delegation_record_pubkey]
    );
    assert_not_subscribed!(
        remote_account_provider,
        &[&delegated_account_pubkey]
    );
}

#[tokio::test]
async fn test_fetch_and_clone_valid_delegated_account_and_account_with_invalid_delegation_record(
) {
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    // Create a delegated account and an account with invalid delegation record
    let delegated_pubkey = random_pubkey();
    let invalid_delegated_pubkey = random_pubkey();

    let delegated_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let invalid_delegated_account = Account {
        lamports: 500_000,
        data: vec![5, 6, 7, 8],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let accounts = [
        (delegated_pubkey, delegated_account.clone()),
        (invalid_delegated_pubkey, invalid_delegated_account.clone()),
    ];

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(accounts, CURRENT_SLOT, validator_keypair.insecure_clone()).await;

    // Add valid delegation record for first account
    add_delegation_record_for(
        &rpc_client,
        delegated_pubkey,
        validator_pubkey,
        account_owner,
    );

    // Add invalid delegation record for second account
    add_invalid_delegation_record_for(&rpc_client, invalid_delegated_pubkey);

    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[delegated_pubkey, invalid_delegated_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");

    // Should return an error due to invalid delegation record
    assert!(result.is_err());
    assert!(matches!(
        result,
        Err(ChainlinkError::InvalidDelegationRecord(_, _))
    ));

    // Verify no accounts were cloned nor subscribed due to the error
    assert!(accounts_bank.get_account(&delegated_pubkey).is_none());
    assert!(accounts_bank
        .get_account(&invalid_delegated_pubkey)
        .is_none());

    assert_not_subscribed!(
        remote_account_provider,
        &[&invalid_delegated_pubkey, &delegated_pubkey]
    );
}

#[tokio::test]
async fn test_deleg_record_stale() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const INITIAL_DELEG_RECORD_SLOT: u64 = CURRENT_SLOT - 10;

    // The account to clone is up to date
    let account_pubkey = random_pubkey();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };
    let FetcherTestCtx {
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Add delegation record which is stale (10 slots behind)
    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );
    rpc_client
        .account_override_slot(&deleg_record_pubkey, INITIAL_DELEG_RECORD_SLOT);

    // Initially we should not be able to clone the account since we cannot
    // find a valid delegation record (up to date the same way the account is)
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");

    // Should return a result indicating missing  delegation record
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap().missing_delegation_record,
        vec![(account_pubkey, CURRENT_SLOT)]
    );

    // After the RPC provider updates the delegation record and has it available
    // at the required slot then all is ok
    rpc_client.account_override_slot(&deleg_record_pubkey, CURRENT_SLOT);
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;
    debug!(result = ?result, "Test result after updating delegation record");
    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}

#[tokio::test]
async fn test_account_stale() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const INITIAL_ACC_SLOT: u64 = CURRENT_SLOT - 10;

    // The account to clone starts stale (10 slots behind)
    let account_pubkey = random_pubkey();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };
    let FetcherTestCtx {
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Override account slot to make it stale
    rpc_client.account_override_slot(&account_pubkey, INITIAL_ACC_SLOT);

    // Add delegation record which is up to date
    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    // Initially we should not be able to clone the account since the account
    // is stale (delegation record is up to date but account is behind)
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");

    // Should return a result indicating the account needs to be updated
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap().not_found_on_chain,
        vec![(account_pubkey, CURRENT_SLOT)]
    );

    // After the RPC provider updates the account to the current slot
    rpc_client.account_override_slot(&account_pubkey, CURRENT_SLOT);
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;
    debug!(result = ?result, "Test result after updating account");
    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}

#[tokio::test]
async fn test_delegation_record_unsub_race_condition_prevention() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Add delegation record
    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    // Test the race condition prevention:
    // 1. Start first operation that will fetch and subscribe to delegation record
    // 2. While first operation is in progress, start second operation for same account
    // 3. When first operation tries to unsubscribe, it should detect pending request and skip unsubscription
    // 4. Second operation should complete successfully

    // Use a shared FetchCloner to test deduplication
    // Helper function to spawn a fetch_and_clone task with shared FetchCloner
    let spawn_fetch_task = |fetch_cloner: &Arc<FetchCloner<_, _, _, _>>| {
        let fetch_cloner = fetch_cloner.clone();
        tokio::spawn(async move {
            fetch_cloner
                .fetch_and_clone_accounts_with_dedup(
                    &[account_pubkey],
                    None,
                    None,
                    AccountFetchOrigin::GetAccount,
                    None,
                )
                .await
        })
    };

    let fetch_cloner = Arc::new(fetch_cloner);

    // Start multiple concurrent operations on the same account
    let task1 = spawn_fetch_task(&fetch_cloner);
    let task2 = spawn_fetch_task(&fetch_cloner);
    let task3 = spawn_fetch_task(&fetch_cloner);

    // Wait for all operations to complete
    let (result0, result1, result2) =
        tokio::try_join!(task1, task2, task3).unwrap();

    // All operations should succeed (no race condition should cause failures)
    let results = [result0, result1, result2];
    for (i, result) in results.into_iter().enumerate() {
        assert!(result.is_ok(), "Operation {i} failed: {result:?}");
    }

    assert!(accounts_bank.get_account(&account_pubkey).is_some());

    assert_not_subscribed!(
        remote_account_provider,
        &[&account_pubkey, &deleg_record_pubkey]
    );
}

#[tokio::test]
async fn test_fetch_and_clone_with_dedup_concurrent_requests() {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let account = Account {
        lamports: 2_000_000,
        data: vec![5, 6, 7, 8],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let fetch_cloner = Arc::new(fetch_cloner);

    // Helper function to spawn fetch task with deduplication
    let spawn_fetch_task = || {
        let fetch_cloner = fetch_cloner.clone();
        tokio::spawn(async move {
            fetch_cloner
                .fetch_and_clone_accounts_with_dedup(
                    &[account_pubkey],
                    None,
                    None,
                    AccountFetchOrigin::GetAccount,
                    None,
                )
                .await
        })
    };

    // Spawn multiple concurrent requests for the same account
    let task1 = spawn_fetch_task();
    let task2 = spawn_fetch_task();

    // Both should succeed
    let (result1, result2) = tokio::try_join!(task1, task2).unwrap();
    assert!(result1.is_ok());
    assert!(result2.is_ok());

    // Verify deduplication: should only fetch the account once despite concurrent requests
    assert_eq!(
        fetch_cloner.fetch_count(),
        1,
        "Expected exactly 1 fetch operation for the same account requested concurrently, got {}",
        fetch_cloner.fetch_count()
    );

    // Account should be cloned (only once)
    assert_cloned_undelegated_account!(
        accounts_bank,
        account_pubkey,
        account,
        CURRENT_SLOT,
        account_owner
    );
}

#[tokio::test]
async fn test_undelegation_requested_subscription_behavior() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    // Initially fetch and clone the delegated account
    // This should result in no active subscription since it's delegated to us
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;
    assert!(result.is_ok());

    // Verify account was cloned and is marked as delegated
    assert_cloned_delegated_account!(
        accounts_bank,
        account_pubkey,
        account,
        CURRENT_SLOT,
        account_owner
    );

    // Initially, delegated accounts to us should NOT be subscribed
    assert_not_subscribed!(remote_account_provider, &[&account_pubkey]);

    // Now simulate undelegation request - this should start subscription
    fetch_cloner
        .subscribe_to_account(&account_pubkey)
        .await
        .expect("Failed to subscribe to account for undelegation");

    assert_subscribed!(remote_account_provider, &[&account_pubkey]);
}

#[tokio::test]
async fn test_delegated_authoritative_skip_unsubscribes_subscription() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let delegated_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        fetch_cloner,
        subscription_tx,
        ..
    } = setup(
        [(account_pubkey, delegated_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    // Clone delegated account into bank (authoritative local delegated state).
    fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await
        .expect("delegated account fetch should succeed");
    assert_cloned_delegated_account!(
        accounts_bank,
        account_pubkey,
        delegated_account.clone(),
        CURRENT_SLOT,
        account_owner
    );

    // Simulate undelegation-tracking subscription being active.
    fetch_cloner
        .subscribe_to_account(&account_pubkey)
        .await
        .expect("failed to subscribe delegated account");
    assert_subscribed!(remote_account_provider, &[&account_pubkey]);

    // Send a newer plain update; delegated authoritative-skip path should still unsubscribe.
    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };
    let chain_update = Account {
        lamports: 900_000,
        data: vec![9, 9, 9, 9],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };
    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: account_pubkey,
            account: RemoteAccount::from_fresh_account(
                chain_update,
                CURRENT_SLOT + 1,
                RemoteAccountUpdateSource::Subscription,
            ),
        })
        .await
        .unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(500);
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if !remote_account_provider.is_watching(&account_pubkey) {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for delegated account unsubscribe");

    assert_not_subscribed!(remote_account_provider, &[&account_pubkey]);

    // Ensure we did not overwrite the local delegated account state.
    assert_cloned_delegated_account!(
        accounts_bank,
        account_pubkey,
        delegated_account,
        CURRENT_SLOT,
        account_owner
    );
}

#[tokio::test]
async fn test_parallel_fetch_prevention_multiple_accounts() {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    // Create multiple accounts that will be fetched in parallel
    let account1_pubkey = random_pubkey();
    let account2_pubkey = random_pubkey();
    let account3_pubkey = random_pubkey();

    let account1 = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let account2 = Account {
        lamports: 2_000_000,
        data: vec![4, 5, 6],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let account3 = Account {
        lamports: 3_000_000,
        data: vec![7, 8, 9],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let accounts = [
        (account1_pubkey, account1.clone()),
        (account2_pubkey, account2.clone()),
        (account3_pubkey, account3.clone()),
    ];

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        ..
    } = setup(accounts, CURRENT_SLOT, validator_keypair.insecure_clone()).await;

    // Use shared FetchCloner to test deduplication across multiple accounts
    // Spawn multiple concurrent requests for overlapping sets of accounts
    let all_accounts = vec![account1_pubkey, account2_pubkey, account3_pubkey];
    let accounts_12 = vec![account1_pubkey, account2_pubkey];
    let accounts_23 = vec![account2_pubkey, account3_pubkey];

    let fetch_cloner = Arc::new(fetch_cloner);

    // Helper function to spawn fetch task with deduplication
    let spawn_fetch_task = |accounts: Vec<Pubkey>| {
        let fetch_cloner = fetch_cloner.clone();
        tokio::spawn(async move {
            fetch_cloner
                .fetch_and_clone_accounts_with_dedup(
                    &accounts,
                    None,
                    None,
                    AccountFetchOrigin::GetAccount,
                    None,
                )
                .await
        })
    };

    let task1 = spawn_fetch_task(all_accounts);
    let task2 = spawn_fetch_task(accounts_12);
    let task3 = spawn_fetch_task(accounts_23);

    // All operations should succeed despite overlapping account requests
    let (result1, result2, result3) =
        tokio::try_join!(task1, task2, task3).unwrap();

    assert!(result1.is_ok(), "Task 1 failed: {result1:?}");
    assert!(result2.is_ok(), "Task 2 failed: {result2:?}");
    assert!(result3.is_ok(), "Task 3 failed: {result3:?}");

    // Verify deduplication: should only fetch 3 unique accounts once each despite overlapping requests
    assert_eq!(fetch_cloner.fetch_count(), 3,);

    // All accounts should be cloned exactly once
    assert_cloned_undelegated_account!(
        accounts_bank,
        account1_pubkey,
        account1,
        CURRENT_SLOT,
        account_owner
    );
    assert_cloned_undelegated_account!(
        accounts_bank,
        account2_pubkey,
        account2,
        CURRENT_SLOT,
        account_owner
    );
    assert_cloned_undelegated_account!(
        accounts_bank,
        account3_pubkey,
        account3,
        CURRENT_SLOT,
        account_owner
    );
}

// -----------------
// Marked Non Existing Accounts
// -----------------
#[tokio::test]
async fn test_fetch_with_some_acounts_marked_as_empty_if_not_found() {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    // Create one existing account and one non-existing account
    let existing_account_pubkey = random_pubkey();
    let marked_non_existing_account_pubkey = random_pubkey();
    let unmarked_non_existing_account_pubkey = random_pubkey();

    let existing_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };
    let accounts = [(existing_account_pubkey, existing_account.clone())];

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        remote_account_provider,
        ..
    } = setup(accounts, CURRENT_SLOT, validator_keypair.insecure_clone()).await;

    // Configure fetch_cloner to mark some accounts as empty if not found
    fetch_cloner
        .fetch_and_clone_accounts(
            &[
                existing_account_pubkey,
                marked_non_existing_account_pubkey,
                unmarked_non_existing_account_pubkey,
            ],
            Some(&[marked_non_existing_account_pubkey]),
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await
        .expect("Fetch and clone failed");

    // Existing account should be cloned normally
    assert_cloned_undelegated_account!(
        accounts_bank,
        existing_account_pubkey,
        existing_account,
        CURRENT_SLOT,
        account_owner
    );

    // Non marked account should not be cloned
    assert_not_cloned!(accounts_bank, &[unmarked_non_existing_account_pubkey]);

    // Marked non-existing account should be cloned as empty
    assert_cloned_undelegated_account!(
        accounts_bank,
        marked_non_existing_account_pubkey,
        Account {
            lamports: 0,
            data: vec![],
            owner: Pubkey::default(),
            executable: false,
            rent_epoch: 0,
        },
        CURRENT_SLOT,
        system_program::id()
    );
    assert_subscribed_without_delegation_record!(
        remote_account_provider,
        &[&marked_non_existing_account_pubkey]
    );
}

#[tokio::test]
async fn test_confined_delegation_behavior() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    // Account 1: Delegated to validator authority -> Not confined
    let account1_pubkey = random_pubkey();
    let account1 = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3],
        owner: dlp_api::id(), // Owned by DLP initially (as it is delegated)
        executable: false,
        rent_epoch: 0,
    };

    // Account 2: Delegated to default pubkey -> Confined
    let account2_pubkey = random_pubkey();
    let account2 = Account {
        lamports: 2_000_000,
        data: vec![4, 5, 6],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        rpc_client,
        ..
    } = setup(
        [
            (account1_pubkey, account1.clone()),
            (account2_pubkey, account2.clone()),
        ],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Add delegation record for Account 1 (Authority = Validator)
    add_delegation_record_for(
        &rpc_client,
        account1_pubkey,
        validator_pubkey,
        account_owner,
    );

    // Add delegation record for Account 2 (Authority = Default/System)
    add_delegation_record_for(
        &rpc_client,
        account2_pubkey,
        Pubkey::default(),
        account_owner,
    );

    // Fetch and clone both accounts
    fetch_cloner
        .fetch_and_clone_accounts(
            &[account1_pubkey, account2_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await
        .expect("Failed to fetch and clone accounts");

    // Verify not confined Account
    let cloned_account1 = accounts_bank
        .get_account(&account1_pubkey)
        .expect("Account 1 not found");
    assert!(cloned_account1.delegated(), "Account 1 should be delegated");
    assert!(
        !cloned_account1.confined(),
        "Account 1 (delegated to validator) should NOT be confined"
    );
    assert_eq!(cloned_account1.owner(), &account_owner);

    // Verify confined Account
    let cloned_account2 = accounts_bank
        .get_account(&account2_pubkey)
        .expect("Account 2 not found");
    assert!(
        cloned_account2.delegated(),
        "Account 2 should be delegated (to us, via confinement)"
    );
    assert!(
        cloned_account2.confined(),
        "Account 2 (delegated to default) SHOULD be confined"
    );
    assert_eq!(cloned_account2.owner(), &account_owner);
}

#[tokio::test]
async fn test_fetch_and_clone_undelegating_account_that_is_closed_on_chain() {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_pubkey = random_pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    // The account exists in the bank (undelegating) but is closed on chain
    let account_in_bank = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    // Setup with NO accounts on chain
    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        remote_account_provider,
        ..
    } = setup(
        std::iter::empty::<(Pubkey, Account)>(),
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Insert account into bank and mark as undelegating
    accounts_bank
        .insert(account_pubkey, AccountSharedData::from(account_in_bank));
    accounts_bank.set_undelegating(&account_pubkey, true);

    // Fetch and clone - should detect closed account and clone empty account
    let result = fetch_cloner
        .fetch_and_clone_accounts_with_dedup(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    debug!(result = ?result, "Test completed");
    assert!(result.is_ok());

    // Account should be replaced with empty account in bank
    let cloned_account = accounts_bank.get_account(&account_pubkey);
    assert!(cloned_account.is_some());
    let cloned_account = cloned_account.unwrap();

    assert_eq!(cloned_account.lamports(), 0);
    assert!(cloned_account.data().is_empty());
    assert_eq!(*cloned_account.owner(), system_program::id());

    // Should be subscribed
    assert_subscribed_without_delegation_record!(
        remote_account_provider,
        &[&account_pubkey]
    );
}

#[tokio::test]
async fn test_auto_airdrop_uses_non_stale_remote_slot_from_bank_account() {
    init_logger();
    let validator_keypair = Keypair::new();
    let payer_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const LOCAL_SLOT: u64 = 250;
    const AIRDROP_LAMPORTS: u64 = 1_000_000_000;

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        ..
    } = setup(
        std::iter::empty::<(Pubkey, Account)>(),
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let mut empty_local_account =
        AccountSharedData::new(0, 0, &system_program::id());
    empty_local_account.set_remote_slot(LOCAL_SLOT);
    accounts_bank.insert(payer_pubkey, empty_local_account);

    fetch_cloner
        .airdrop_account_if_empty(payer_pubkey, AIRDROP_LAMPORTS)
        .await
        .expect("airdrop should succeed");

    let payer_after = accounts_bank
        .get_account(&payer_pubkey)
        .expect("payer should exist in bank");
    assert_eq!(payer_after.lamports(), AIRDROP_LAMPORTS);
    assert_eq!(payer_after.remote_slot(), LOCAL_SLOT);
    assert_eq!(*payer_after.owner(), system_program::id());
}

#[tokio::test]
async fn test_auto_airdrop_uses_chain_slot_when_account_not_in_bank() {
    init_logger();
    let validator_keypair = Keypair::new();
    let payer_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const AIRDROP_LAMPORTS: u64 = 1_000_000_000;

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        ..
    } = setup(
        std::iter::empty::<(Pubkey, Account)>(),
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    fetch_cloner
        .airdrop_account_if_empty(payer_pubkey, AIRDROP_LAMPORTS)
        .await
        .expect("airdrop should succeed");

    let payer_after = accounts_bank
        .get_account(&payer_pubkey)
        .expect("payer should exist in bank");
    assert_eq!(payer_after.lamports(), AIRDROP_LAMPORTS);
    assert_eq!(payer_after.remote_slot(), CURRENT_SLOT);
    assert_eq!(*payer_after.owner(), system_program::id());
}

// -----------------
// Allowed Programs Tests
// -----------------

#[tokio::test]
async fn test_allowed_programs_filters_programs() {
    init_logger();
    let validator_keypair = Keypair::new();
    let program_id_allowed = random_pubkey();
    let program_id_blocked = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let program_account_allowed = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_sdk_ids::bpf_loader::id(),
        executable: true,
        rent_epoch: 0,
    };

    let program_account_blocked = Account {
        lamports: 1_000_000,
        data: vec![5, 6, 7, 8],
        owner: solana_sdk_ids::bpf_loader::id(),
        executable: true,
        rent_epoch: 0,
    };

    let setup_accounts = vec![
        (program_id_allowed, program_account_allowed),
        (program_id_blocked, program_account_blocked),
    ];

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner: _fetch_cloner,
        remote_account_provider,
        ..
    } = setup(
        setup_accounts.into_iter(),
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Create FetchCloner with only one program allowed
    let (_subscription_tx, subscription_rx) = mpsc::channel(100);
    let cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
    let allowed_programs = Some(vec![AllowedProgram {
        id: program_id_allowed,
    }]);
    let fetch_cloner = FetchCloner::new(
        &remote_account_provider,
        &accounts_bank,
        &cloner,
        validator_keypair.insecure_clone(),
        random_pubkey(),
        subscription_rx,
        allowed_programs,
    );

    // Fetch and clone both programs
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[program_id_allowed, program_id_blocked],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            Some(&[program_id_allowed, program_id_blocked]),
        )
        .await;

    debug!(result = ?result, "Test completed");
    assert!(result.is_ok());

    // The allowed program should be in the bank
    assert!(
        accounts_bank.get_account(&program_id_allowed).is_some(),
        "Allowed program should be in the bank"
    );

    // The blocked program should NOT be in the bank
    assert!(
        accounts_bank.get_account(&program_id_blocked).is_none(),
        "Blocked program should NOT be in the bank"
    );
}

#[tokio::test]
async fn test_allowed_programs_none_allows_all() {
    init_logger();
    let validator_keypair = Keypair::new();
    let program_id1 = random_pubkey();
    let program_id2 = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let program_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_sdk_ids::bpf_loader::id(),
        executable: true,
        rent_epoch: 0,
    };

    let setup_accounts = vec![
        (program_id1, program_account.clone()),
        (program_id2, program_account),
    ];

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner: _fetch_cloner,
        remote_account_provider,
        ..
    } = setup(
        setup_accounts.into_iter(),
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Create FetchCloner with NO allowed_programs restriction (None)
    let (_subscription_tx, subscription_rx) = mpsc::channel(100);
    let cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
    let fetch_cloner = FetchCloner::new(
        &remote_account_provider,
        &accounts_bank,
        &cloner,
        validator_keypair.insecure_clone(),
        random_pubkey(),
        subscription_rx,
        None, // No restriction
    );

    // Fetch and clone both programs
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[program_id1, program_id2],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            Some(&[program_id1, program_id2]),
        )
        .await;

    debug!(result = ?result, "Test completed");
    assert!(result.is_ok());

    // Both programs should be in the bank
    assert!(
        accounts_bank.get_account(&program_id1).is_some(),
        "Program 1 should be in the bank"
    );
    assert!(
        accounts_bank.get_account(&program_id2).is_some(),
        "Program 2 should be in the bank"
    );
}

#[tokio::test]
async fn test_allowed_programs_empty_allows_all() {
    init_logger();
    let validator_keypair = Keypair::new();
    let program_id1 = random_pubkey();
    let program_id2 = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let program_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_sdk_ids::bpf_loader::id(),
        executable: true,
        rent_epoch: 0,
    };

    let setup_accounts = vec![
        (program_id1, program_account.clone()),
        (program_id2, program_account),
    ];

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner: _fetch_cloner,
        remote_account_provider,
        ..
    } = setup(
        setup_accounts.into_iter(),
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Create FetchCloner with an EMPTY allowed_programs list
    let (_subscription_tx, subscription_rx) = mpsc::channel(100);
    let cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
    let allowed_programs = Some(vec![]); // Empty list
    let fetch_cloner = FetchCloner::new(
        &remote_account_provider,
        &accounts_bank,
        &cloner,
        validator_keypair.insecure_clone(),
        random_pubkey(),
        subscription_rx,
        allowed_programs,
    );

    // Fetch and clone both programs
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[program_id1, program_id2],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            Some(&[program_id1, program_id2]),
        )
        .await;

    debug!(result = ?result, "Test completed");
    assert!(result.is_ok());

    // Both programs should be in the bank (empty list is treated as unrestricted)
    assert!(
        accounts_bank.get_account(&program_id1).is_some(),
        "Program 1 should be in the bank (empty allowed_programs allows all)"
    );
    assert!(
        accounts_bank.get_account(&program_id2).is_some(),
        "Program 2 should be in the bank (empty allowed_programs allows all)"
    );
}

// -----------------
// Program Subscription Tests for Delegated Accounts
// -----------------

#[tokio::test]
async fn test_subscribe_to_original_owner_program_on_delegated_account_fetch() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Add delegation record with original owner
    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    // Fetch and clone the delegated account
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    assert!(result.is_ok());

    // Verify account was cloned and marked as delegated
    assert_cloned_delegated_account!(
        accounts_bank,
        account_pubkey,
        account,
        CURRENT_SLOT,
        account_owner
    );

    // Verify that we subscribed to the original owner program
    let pubsub_client = remote_account_provider.pubsub_client();
    let subscribed_programs = pubsub_client.subscribed_program_ids();
    assert!(
        subscribed_programs.contains(&account_owner),
        "Should subscribe to original owner program {}, got: {:?}",
        account_owner,
        subscribed_programs
    );
}

#[tokio::test]
async fn test_no_program_subscription_for_undelegated_account() {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let undelegated_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, undelegated_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Verify that initially we don't subscribe to any program
    let pubsub_client = remote_account_provider.pubsub_client();
    let initial_programs = pubsub_client.subscribed_program_ids();
    assert!(
        initial_programs.is_empty(),
        "Should have no program subscriptions initially"
    );

    // Fetch and clone the undelegated account
    let result = fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;

    assert!(result.is_ok());

    // Verify account was cloned but not delegated
    assert_cloned_undelegated_account!(
        accounts_bank,
        account_pubkey,
        undelegated_account,
        CURRENT_SLOT,
        account_owner
    );

    // Still no program subscriptions since it wasn't delegated
    let programs_after_fetch = pubsub_client.subscribed_program_ids();
    assert!(
        programs_after_fetch.is_empty(),
        "Should have no program subscriptions after fetching undelegated account"
    );
}

#[allow(clippy::too_many_arguments)]
async fn send_subscription_update_and_get_subscribed_programs(
    remote_account_provider: &Arc<
        RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
    >,
    accounts_bank: &Arc<AccountsBankStub>,
    subscription_tx: &mpsc::Sender<ForwardedSubscriptionUpdate>,
    account_pubkey: Pubkey,
    bank_account: Account,
    update_account: Account,
    slot: u64,
    expected_program_id: Option<Pubkey>,
) -> std::collections::HashSet<Pubkey> {
    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    accounts_bank.insert(account_pubkey, AccountSharedData::from(bank_account));

    let pubsub_client = remote_account_provider.pubsub_client();
    let initial_programs = pubsub_client.subscribed_program_ids();
    assert!(
        initial_programs.is_empty(),
        "Should have no program subscriptions initially"
    );

    let remote_account = RemoteAccount::from_fresh_account(
        update_account,
        slot,
        RemoteAccountUpdateSource::Subscription,
    );
    let update = ForwardedSubscriptionUpdate {
        pubkey: account_pubkey,
        account: remote_account,
    };
    subscription_tx.send(update).await.unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(200);

    let result = tokio::time::timeout(TIMEOUT, async {
        loop {
            let subscribed = pubsub_client.subscribed_program_ids();
            match expected_program_id {
                Some(expected) if subscribed.contains(&expected) => {
                    return subscribed;
                }
                None if !subscribed.is_empty() => {
                    return subscribed;
                }
                _ => {}
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await;

    match result {
        Ok(subscribed) => subscribed,
        Err(_) if expected_program_id.is_some() => {
            panic!(
                "Timeout waiting for program subscription {:?}",
                expected_program_id
            )
        }
        Err(_) => pubsub_client.subscribed_program_ids(),
    }
}

#[tokio::test]
async fn test_subscribe_to_original_owner_program_on_delegated_account_subscription_update(
) {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let delegated_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        subscription_tx,
        ..
    } = setup(
        [(account_pubkey, delegated_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    let bank_account = Account {
        lamports: 500_000,
        data: vec![0, 0, 0, 0],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let subscribed_programs =
        send_subscription_update_and_get_subscribed_programs(
            &remote_account_provider,
            &accounts_bank,
            &subscription_tx,
            account_pubkey,
            bank_account,
            delegated_account,
            CURRENT_SLOT,
            Some(account_owner),
        )
        .await;

    assert!(
        subscribed_programs.contains(&account_owner),
        "Should subscribe to original owner program {} via subscription update, got: {:?}",
        account_owner,
        subscribed_programs
    );
}

#[tokio::test]
async fn test_no_program_subscription_for_undelegated_account_subscription_update(
) {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let undelegated_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        subscription_tx,
        ..
    } = setup(
        [(account_pubkey, undelegated_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let subscribed_programs =
        send_subscription_update_and_get_subscribed_programs(
            &remote_account_provider,
            &accounts_bank,
            &subscription_tx,
            account_pubkey,
            undelegated_account.clone(),
            undelegated_account,
            CURRENT_SLOT,
            None,
        )
        .await;

    assert!(
        subscribed_programs.is_empty(),
        "Should have no program subscriptions for undelegated account subscription update, got: {:?}",
        subscribed_programs
    );
}

#[tokio::test]
async fn test_fetch_and_clone_non_raw_eata_owned_account_as_delegated() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const DATA_LEN: usize = 9728;

    let (account, wallet_owner, mint) =
        create_non_raw_eata_owned_account(account_pubkey, DATA_LEN);
    let ata_pubkey = derive_ata(&wallet_owner, &mint);

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        rpc_client,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
    );

    fetch_cloner
        .fetch_and_clone_accounts(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await
        .expect("Failed to fetch and clone delegated EATA-owned account");

    let cloned_account = accounts_bank
        .get_account(&account_pubkey)
        .expect("account should be cloned");
    assert_eq!(*cloned_account.owner(), EATA_PROGRAM_ID);
    assert!(cloned_account.delegated());
    assert!(!cloned_account.confined());
    assert_eq!(cloned_account.remote_slot(), CURRENT_SLOT);
    assert_eq!(cloned_account.data().len(), DATA_LEN);
    assert!(
        accounts_bank.get_account(&ata_pubkey).is_none(),
        "non-raw EATA-owned account must not project an ATA clone"
    );
}

#[tokio::test]
async fn test_non_raw_eata_owned_account_subscription_update_stays_delegated() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const DATA_LEN: usize = 9728;

    let (account, wallet_owner, mint) =
        create_non_raw_eata_owned_account(account_pubkey, DATA_LEN);
    let ata_pubkey = derive_ata(&wallet_owner, &mint);

    let FetcherTestCtx {
        accounts_bank,
        rpc_client,
        subscription_tx,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
    );

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: account_pubkey,
            account: RemoteAccount::from_fresh_account(
                account,
                CURRENT_SLOT,
                RemoteAccountUpdateSource::Subscription,
            ),
        })
        .await
        .unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(500);
    tokio::time::timeout(TIMEOUT, async {
        while accounts_bank.get_account(&account_pubkey).is_none() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for delegated EATA-owned subscription update");

    let cloned_account = accounts_bank
        .get_account(&account_pubkey)
        .expect("account should be cloned from subscription update");
    assert_eq!(*cloned_account.owner(), EATA_PROGRAM_ID);
    assert!(cloned_account.delegated());
    assert!(!cloned_account.confined());
    assert_eq!(cloned_account.remote_slot(), CURRENT_SLOT);
    assert_eq!(cloned_account.data().len(), DATA_LEN);
    assert!(
        accounts_bank.get_account(&ata_pubkey).is_none(),
        "non-raw EATA-owned account subscription update must not project an ATA clone"
    );
}

#[tokio::test]
async fn test_discovered_dlp_owned_account_without_delegation_record_falls_back(
) {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let dlp_owned_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        accounts_bank,
        subscription_tx,
        ..
    } = setup(
        [(account_pubkey, dlp_owned_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: account_pubkey,
            account: RemoteAccount::from_fresh_account(
                dlp_owned_account,
                CURRENT_SLOT,
                RemoteAccountUpdateSource::Subscription,
            ),
        })
        .await
        .unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(200);
    tokio::time::timeout(TIMEOUT, async {
        while accounts_bank.get_account(&account_pubkey).is_none() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect(
        "timed out waiting for fallback clone of discovered DLP-owned account",
    );

    let cloned_account = accounts_bank
        .get_account(&account_pubkey)
        .expect("account should be cloned by fallback subscription path");
    assert_eq!(*cloned_account.owner(), dlp_api::id());
    assert!(!cloned_account.delegated());
    assert_eq!(cloned_account.remote_slot(), CURRENT_SLOT);
}

#[tokio::test]
async fn test_discovered_dlp_owned_account_delegated_elsewhere_is_ignored() {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    let other_validator = random_pubkey();
    let account_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let delegated_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        accounts_bank,
        rpc_client,
        subscription_tx,
        ..
    } = setup(
        [(account_pubkey, delegated_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        other_validator,
        account_owner,
    );

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: account_pubkey,
            account: RemoteAccount::from_fresh_account(
                delegated_account,
                CURRENT_SLOT,
                RemoteAccountUpdateSource::Subscription,
            ),
        })
        .await
        .unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(200);
    let cloned = tokio::time::timeout(TIMEOUT, async {
        loop {
            if accounts_bank.get_account(&account_pubkey).is_some() {
                return true;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap_or(false);

    assert!(
        !cloned,
        "subscription auto-discovery should ignore accounts delegated to another validator"
    );
}

#[tokio::test]
async fn test_out_of_order_delegated_eata_subscription_update_still_projects_ata(
) {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const AMOUNT: u64 = 777;

    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_account = create_eata_account(&wallet_owner, &mint, AMOUNT, true);

    let FetcherTestCtx {
        accounts_bank,
        rpc_client,
        subscription_tx,
        ..
    } = setup(
        [(eata_pubkey, eata_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
    );

    let mut in_bank_eata = AccountSharedData::from(eata_account.clone());
    in_bank_eata.set_owner(EATA_PROGRAM_ID);
    in_bank_eata.set_remote_slot(CURRENT_SLOT);
    accounts_bank.insert(eata_pubkey, in_bank_eata);

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: eata_pubkey,
            account: RemoteAccount::from_fresh_account(
                eata_account,
                CURRENT_SLOT,
                RemoteAccountUpdateSource::Subscription,
            ),
        })
        .await
        .unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(500);
    tokio::time::timeout(TIMEOUT, async {
        while accounts_bank.get_account(&ata_pubkey).is_none() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for ATA projection from out-of-order delegated eATA update");

    let projected_ata = accounts_bank
        .get_account(&ata_pubkey)
        .expect("ATA should still be projected from delegated eATA update");
    assert!(projected_ata.delegated());
    assert_eq!(projected_ata.remote_slot(), CURRENT_SLOT);
}

#[tokio::test]
async fn test_subscription_update_with_delegation_actions_clones_dependencies()
{
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_pubkey = random_pubkey();
    let action_program_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let delegated_account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };
    let action_program_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_sdk_ids::bpf_loader::id(),
        executable: true,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        accounts_bank,
        rpc_client,
        subscription_tx,
        ..
    } = setup(
        [
            (account_pubkey, delegated_account.clone()),
            (action_program_pubkey, action_program_account),
        ],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_with_actions_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        system_program::id(),
        action_program_pubkey,
    );

    let mut stale_in_bank = AccountSharedData::from(delegated_account.clone());
    stale_in_bank.set_remote_slot(CURRENT_SLOT - 1);
    accounts_bank.insert(account_pubkey, stale_in_bank);

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: account_pubkey,
            account: RemoteAccount::from_fresh_account(
                delegated_account,
                CURRENT_SLOT,
                RemoteAccountUpdateSource::Subscription,
            ),
        })
        .await
        .unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(500);
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let delegated_account_slot = accounts_bank
                .get_account(&account_pubkey)
                .map(|account| account.remote_slot());
            let has_action_program =
                accounts_bank.get_account(&action_program_pubkey).is_some();
            if delegated_account_slot == Some(CURRENT_SLOT)
                && has_action_program
            {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for subscription action dependencies");

    let cloned_account = accounts_bank
        .get_account(&account_pubkey)
        .expect("delegated account should be cloned from subscription update");
    assert!(cloned_account.delegated());
    assert_eq!(cloned_account.remote_slot(), CURRENT_SLOT);
    assert!(
        accounts_bank.get_account(&action_program_pubkey).is_some(),
        "subscription update should clone action program dependencies before running post-delegation actions",
    );
}

#[tokio::test]
async fn test_delegated_eata_subscription_update_clones_raw_eata_and_projects_ata(
) {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const AMOUNT: u64 = 777;

    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_account = create_eata_account(&wallet_owner, &mint, AMOUNT, true);

    let FetcherTestCtx {
        accounts_bank,
        rpc_client,
        subscription_tx,
        ..
    } = setup(
        [(eata_pubkey, eata_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
    );

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: eata_pubkey,
            account: RemoteAccount::from_fresh_account(
                eata_account,
                CURRENT_SLOT,
                RemoteAccountUpdateSource::Subscription,
            ),
        })
        .await
        .unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(500);
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let has_eata = accounts_bank.get_account(&eata_pubkey).is_some();
            let has_ata = accounts_bank.get_account(&ata_pubkey).is_some();
            if has_eata && has_ata {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for eATA + ATA clones from subscription update");

    let cloned_eata = accounts_bank
        .get_account(&eata_pubkey)
        .expect("eATA should be cloned from subscription update");
    assert_eq!(*cloned_eata.owner(), EATA_PROGRAM_ID);
    assert!(!cloned_eata.delegated());
    assert_eq!(cloned_eata.remote_slot(), CURRENT_SLOT);

    let projected_ata = accounts_bank.get_account(&ata_pubkey).expect(
        "ATA should be projected and cloned from delegated eATA update",
    );
    assert!(projected_ata.delegated());
    assert_eq!(projected_ata.remote_slot(), CURRENT_SLOT);

    let ata_data = projected_ata.data();
    assert!(
        ata_data.len() >= 72,
        "Projected ATA data must contain mint/owner/amount"
    );
    let projected_mint =
        Pubkey::new_from_array(ata_data[0..32].try_into().unwrap());
    let projected_owner =
        Pubkey::new_from_array(ata_data[32..64].try_into().unwrap());
    let projected_amount =
        u64::from_le_bytes(ata_data[64..72].try_into().unwrap());
    assert_eq!(projected_mint, mint);
    assert_eq!(projected_owner, wallet_owner);
    assert_eq!(projected_amount, AMOUNT);
}

#[tokio::test]
async fn test_delegated_eata_update_does_not_override_delegated_ata_in_bank() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const CHAIN_EATA_AMOUNT: u64 = 777;
    const LOCAL_ATA_AMOUNT: u64 = 999;

    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_account =
        create_eata_account(&wallet_owner, &mint, CHAIN_EATA_AMOUNT, true);

    let FetcherTestCtx {
        accounts_bank,
        rpc_client,
        subscription_tx,
        ..
    } = setup(
        [(eata_pubkey, eata_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
    );

    // Simulate local delegated ATA state that was already mutated in the validator.
    let mut local_ata = create_ata_account(&wallet_owner, &mint);
    local_ata.data[64..72].copy_from_slice(&LOCAL_ATA_AMOUNT.to_le_bytes());
    let mut local_ata_shared = AccountSharedData::from(local_ata);
    local_ata_shared.set_remote_slot(CURRENT_SLOT - 1);
    local_ata_shared.set_delegated(true);
    accounts_bank.insert(ata_pubkey, local_ata_shared);

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    // A newer chain update for delegated eATA must not override delegated ATA in bank.
    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: eata_pubkey,
            account: RemoteAccount::from_fresh_account(
                eata_account,
                CURRENT_SLOT,
                RemoteAccountUpdateSource::Subscription,
            ),
        })
        .await
        .unwrap();

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(500);
    tokio::time::timeout(TIMEOUT, async {
        while accounts_bank.get_account(&eata_pubkey).is_none() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for delegated eATA subscription update");

    let ata_after = accounts_bank
        .get_account(&ata_pubkey)
        .expect("ATA should still exist in bank");
    assert!(ata_after.delegated(), "ATA must remain delegated");
    assert_eq!(
        ata_after.remote_slot(),
        CURRENT_SLOT - 1,
        "Delegated ATA should not be overwritten by chain update",
    );

    let ata_data = ata_after.data();
    let ata_amount = u64::from_le_bytes(ata_data[64..72].try_into().unwrap());
    assert_eq!(
        ata_amount, LOCAL_ATA_AMOUNT,
        "Delegated ATA amount should keep local state",
    );
}

#[tokio::test]
async fn test_fetch_subscription_race_duplicate_clone() {
    // This test reproduces the duplicate clone transaction race condition
    // between the on-demand fetch path (fetch_and_clone_accounts_with_dedup)
    // and the subscription update path (process_subscription_update).
    //
    // The race condition: both paths do not coordinate with each other.
    // When an account is being fetched on-demand while a subscription update
    // arrives for the same (pubkey, slot), both paths independently submit
    // clone requests, resulting in duplicate clone transactions.
    //
    // BEFORE FIX: new_clones == 2 (both paths submit independently)
    // AFTER FIX: new_clones == 1 (shared dedup cache prevents duplicates)
    //
    // To reproduce reliably we use:
    // 1. A non-delegated account so the subscription path doesn't get
    //    intercepted by greedy clone (which routes back through the fetch path).
    // 2. A clone delay in the ClonerStub so the fetch path's clone_account()
    //    takes long enough for the subscription path to also pass the bank
    //    slot check before either writes to the bank.

    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    // Non-delegated account (owner is NOT dlp_api::id()) so the subscription
    // path goes through resolve → clone_account() independently instead of
    // being intercepted by maybe_greedily_clone_discovered_delegated_account.
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: account_owner,
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        accounts_bank,
        remote_account_provider,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Create a cloner stub with a delay so clone_account() takes long enough
    // for both paths to pass the bank slot check before either writes.
    let cloner_stub = Arc::new(ClonerStub::new(accounts_bank.clone()));
    cloner_stub
        .set_clone_delay(std::time::Duration::from_millis(200));
    let initial_count = cloner_stub.clone_request_count();

    // Create FetchCloner with our tracking cloner stub
    let (subscription_tx, subscription_rx) = mpsc::channel(100);
    let fetch_cloner = FetchCloner::new(
        &remote_account_provider,
        &accounts_bank,
        &cloner_stub,
        validator_keypair.insecure_clone(),
        Pubkey::new_unique(),
        subscription_rx,
        None,
    );

    // PHASE 1: Trigger on-demand fetch (spawned, runs concurrently)
    let fetch_task = {
        let fc = fetch_cloner.clone();
        tokio::spawn(async move {
            fc.fetch_and_clone_accounts_with_dedup(
                &[account_pubkey],
                None,
                None,
                AccountFetchOrigin::GetAccount,
                None,
            )
            .await
        })
    };

    // PHASE 2: Let the fetch path start processing (RPC call completes in
    // the mock almost instantly, but we need it past the bank check).
    // The 200ms clone delay ensures it hasn't written to bank yet.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // PHASE 3: Inject subscription update for the SAME (pubkey, slot).
    // The subscription listener picks this up and processes it through
    // process_subscription_update → resolve → clone_account().
    // Since the fetch path's clone_account() is still sleeping (200ms delay),
    // the account is NOT yet in bank, so the subscription path passes
    // the out-of-order slot check and also calls clone_account().
    let subscription_account =
        crate::remote_account_provider::RemoteAccount::from_fresh_account(
            account.clone(),
            CURRENT_SLOT,
            crate::remote_account_provider::RemoteAccountUpdateSource::Subscription,
        );
    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: account_pubkey,
            account: subscription_account,
        })
        .await
        .unwrap();

    // PHASE 4: Wait for both paths to complete.
    // The fetch path takes ~200ms (clone delay). The subscription path
    // also takes ~200ms. Give enough time for both.
    let fetch_result = fetch_task.await.unwrap();
    assert!(
        fetch_result.is_ok(),
        "Fetch should succeed, got: {:?}",
        fetch_result
    );

    // Wait for the subscription path to finish processing too
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // ASSERTION: Both paths submitted clone requests independently (the bug)
    let clone_requests = cloner_stub.clone_requests();
    let post_count = clone_requests.len();
    let new_clones = post_count - initial_count;

    // Count clones specifically for our account at the same slot
    let same_account_clones = clone_requests
        .iter()
        .filter(|r| {
            r.pubkey == account_pubkey
                && r.account.remote_slot() == CURRENT_SLOT
        })
        .count();

    // EXPECTED: Only 1 clone request should be submitted for the same
    // (pubkey, slot). Without cross-path dedup both the fetch and subscription
    // paths independently call clone_account(), producing 2 requests.
    // This assertion will FAIL until the dedup fix is applied.
    assert_eq!(
        same_account_clones, 1,
        "Expected 1 clone request (dedup should prevent duplicate), got {}. \
         Clone requests: {}",
        same_account_clones,
        clone_requests
            .iter()
            .enumerate()
            .map(|(i, r)| format!(
                "[{}] pubkey={}, slot={}",
                i,
                r.pubkey,
                r.account.remote_slot()
            ))
            .collect::<Vec<_>>()
            .join("; ")
    );

    // Verify account was cloned into bank
    assert!(
        accounts_bank.get_account(&account_pubkey).is_some(),
        "Account should be present in bank"
    );
}
