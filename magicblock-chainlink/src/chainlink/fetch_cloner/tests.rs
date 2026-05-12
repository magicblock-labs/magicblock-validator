use std::{collections::HashMap, sync::Arc, time::Duration};

use dlp_api::state::DelegationRecord;
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
        RemoteAccountUpdateSource,
    },
    testing::{
        accounts::{
            account_shared_with_owner, delegated_account_shared_with_owner,
            delegated_account_shared_with_owner_and_slot,
        },
        cloner_stub::ClonerStub,
        deleg::{
            add_delegation_record_for, add_delegation_record_with_actions_for,
            add_invalid_delegation_record_for, delegation_record_to_vec,
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

fn add_delegation_record_with_slot_for(
    rpc_client: &ChainRpcClientMock,
    pubkey: Pubkey,
    authority: Pubkey,
    owner: Pubkey,
    delegation_slot: u64,
) -> Pubkey {
    let deleg_record_pubkey =
        dlp_api::pda::delegation_record_pda_from_delegated_account(&pubkey);
    let deleg_record = DelegationRecord {
        authority,
        owner,
        delegation_slot,
        lamports: 1_000,
        commit_frequency_ms: 2_000,
    };
    rpc_client.add_account(
        deleg_record_pubkey,
        Account {
            owner: dlp_api::id(),
            data: delegation_record_to_vec(&deleg_record),
            ..Default::default()
        },
    );
    deleg_record_pubkey
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

async fn wait_for_pending_request(
    fetch_cloner: &Arc<
        FetchCloner<
            ChainRpcClientMock,
            ChainPubsubClientMock,
            AccountsBankStub,
            ClonerStub,
        >,
    >,
    pubkey: Pubkey,
) {
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    while !fetch_cloner.has_pending_request(&pubkey)
        && start.elapsed() < timeout
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        fetch_cloner.has_pending_request(&pubkey),
        "pending request should exist for {pubkey}"
    );
}

async fn wait_for_pending_waiter_count(
    fetch_cloner: &Arc<
        FetchCloner<
            ChainRpcClientMock,
            ChainPubsubClientMock,
            AccountsBankStub,
            ClonerStub,
        >,
    >,
    pubkey: Pubkey,
    expected: usize,
) {
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    while fetch_cloner.pending_request_waiter_count(&pubkey) != Some(expected)
        && start.elapsed() < timeout
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fetch_cloner.pending_request_waiter_count(&pubkey),
        Some(expected),
        "pending waiter count for {pubkey} should be {expected}"
    );
}

async fn wait_for_rpc_fetch_activity(
    rpc_client: &ChainRpcClientMock,
    expected_minimum: u64,
) {
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    while rpc_client.single_account_fetches()
        + rpc_client.multi_account_fetches()
        < expected_minimum
        && start.elapsed() < timeout
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        rpc_client.single_account_fetches()
            + rpc_client.multi_account_fetches()
            >= expected_minimum,
        "rpc fetch activity should be at least {expected_minimum}"
    );
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
async fn test_get_account_releases_delegation_record_direct_ref_when_already_watched(
) {
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_pubkey = random_pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account = Account {
        lamports: 1_234,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    remote_account_provider
        .acquire_subscription(
            &deleg_record_pubkey,
            SubscriptionReason::DelegationRecord,
        )
        .await
        .unwrap();

    let (resolved_account, delegation_record, _actions) = fetch_cloner
        .resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
            ForwardedSubscriptionUpdate {
                pubkey: account_pubkey,
                account: RemoteAccount::from_fresh_account(
                    account.clone(),
                    CURRENT_SLOT,
                    RemoteAccountUpdateSource::Subscription,
                ),
            },
        )
        .await;

    let resolved_account = resolved_account.expect("account should resolve");
    let mut expected_account =
        delegated_account_shared_with_owner(&account, account_owner);
    expected_account.set_remote_slot(CURRENT_SLOT);
    assert_eq!(resolved_account, expected_account);
    assert!(delegation_record.is_some());

    remote_account_provider
        .release_single_subscription(
            &deleg_record_pubkey,
            SubscriptionReason::DelegationRecord,
        )
        .await
        .unwrap();

    assert!(
        !remote_account_provider.is_watching(&deleg_record_pubkey),
        "delegation record direct ref should not leak"
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
        .subscribe_to_account_to_track_undelegation(&account_pubkey)
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
        .subscribe_to_account_to_track_undelegation(&account_pubkey)
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

#[tokio::test]
async fn test_program_loader_resolver_error_releases_program_data_refs() {
    use crate::remote_account_provider::program_account::{
        get_loaderv3_get_program_data_address, LOADER_V3,
    };

    init_logger();
    let validator_keypair = Keypair::new();
    let program_pubkey = random_pubkey();
    let program_data_pubkey =
        get_loaderv3_get_program_data_address(&program_pubkey);
    const CURRENT_SLOT: u64 = 100;

    let program_account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: LOADER_V3,
        executable: true,
        rent_epoch: 0,
    };
    let invalid_program_data_account = Account {
        lamports: 1_000_000,
        data: vec![0],
        owner: LOADER_V3,
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        remote_account_provider,
        fetch_cloner,
        ..
    } = setup(
        [
            (program_pubkey, program_account.clone()),
            (program_data_pubkey, invalid_program_data_account),
        ],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let mut program_account_shared = AccountSharedData::from(program_account);
    program_account_shared.set_remote_slot(CURRENT_SLOT);

    program_loader::handle_executable_sub_update(
        &fetch_cloner,
        program_pubkey,
        program_account_shared,
    )
    .await;

    assert!(
        !remote_account_provider.is_watching(&program_data_pubkey),
        "resolver errors must release LoaderV3 program-data subscription refs"
    );
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
async fn test_out_of_order_delegated_eata_update_clones_action_dependencies() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    let action_program_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const AMOUNT: u64 = 777;

    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_account = create_eata_account(&wallet_owner, &mint, AMOUNT, true);
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
            (eata_pubkey, eata_account.clone()),
            (action_program_pubkey, action_program_account),
        ],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_with_actions_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
        action_program_pubkey,
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
        loop {
            let has_ata = accounts_bank.get_account(&ata_pubkey).is_some();
            let has_action_program =
                accounts_bank.get_account(&action_program_pubkey).is_some();
            if has_ata && has_action_program {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect(
        "timed out waiting for projected ATA action dependencies on out-of-order delegated eATA update",
    );

    assert!(
        accounts_bank.get_account(&action_program_pubkey).is_some(),
        "out-of-order projected ATA clone should ensure action dependencies before running post-delegation actions",
    );
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
async fn test_delegated_eata_subscription_update_clones_action_dependencies() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    let action_program_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const AMOUNT: u64 = 777;

    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_account = create_eata_account(&wallet_owner, &mint, AMOUNT, true);
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
            (eata_pubkey, eata_account.clone()),
            (action_program_pubkey, action_program_account),
        ],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_with_actions_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
        action_program_pubkey,
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
            let has_action_program =
                accounts_bank.get_account(&action_program_pubkey).is_some();
            if has_eata && has_ata && has_action_program {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect(
        "timed out waiting for projected ATA action dependencies on delegated eATA update",
    );

    assert!(
        accounts_bank.get_account(&action_program_pubkey).is_some(),
        "projected ATA clone should ensure action dependencies before running post-delegation actions",
    );
}

#[tokio::test]
async fn test_projected_ata_clone_request_from_eata_update_keeps_actions() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    let action_program_pubkey = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_account = create_eata_account(&wallet_owner, &mint, 777, true);

    let FetcherTestCtx {
        fetch_cloner,
        rpc_client,
        ..
    } = setup(
        [(eata_pubkey, eata_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_with_actions_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
        action_program_pubkey,
    );

    let (deleg_record, delegation_actions) = fetch_cloner
        .fetch_and_parse_delegation_record(
            eata_pubkey,
            CURRENT_SLOT,
            AccountFetchOrigin::GetAccount,
        )
        .await
        .expect("delegation record with actions should resolve");

    let mut eata_shared = AccountSharedData::from(eata_account);
    eata_shared.set_remote_slot(CURRENT_SLOT);

    let projected_ata_request = fetch_cloner
        .maybe_build_projected_ata_clone_request_from_eata_sub_update(
            eata_pubkey,
            &eata_shared,
            Some(&deleg_record),
            delegation_actions.as_ref().expect(
                "delegation actions should be parsed for our validator",
            ),
        )
        .expect(
            "delegated eATA update should build projected ATA clone request",
        );

    assert_eq!(projected_ata_request.pubkey, ata_pubkey);
    assert!(
        !projected_ata_request.delegation_actions.is_empty(),
        "projected ATA clone request must preserve post-delegation actions",
    );
}

#[tokio::test]
async fn test_fetch_and_parse_delegation_record_releases_direct_ref_when_already_watched(
) {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let eata_account = create_eata_account(&wallet_owner, &mint, 777, true);

    let FetcherTestCtx {
        remote_account_provider,
        rpc_client,
        fetch_cloner,
        ..
    } = setup(
        [(eata_pubkey, eata_account)],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let delegation_record_pubkey = add_delegation_record_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
    );

    remote_account_provider
        .acquire_subscription(
            &delegation_record_pubkey,
            SubscriptionReason::DelegationRecord,
        )
        .await
        .unwrap();

    let (delegation_record, _) = fetch_cloner
        .fetch_and_parse_delegation_record(
            eata_pubkey,
            CURRENT_SLOT,
            AccountFetchOrigin::GetAccount,
        )
        .await
        .expect("delegation record should resolve");

    assert_eq!(delegation_record.authority, validator_pubkey);
    assert_eq!(delegation_record.owner, EATA_PROGRAM_ID);

    remote_account_provider
        .release_single_subscription(
            &delegation_record_pubkey,
            SubscriptionReason::DelegationRecord,
        )
        .await
        .unwrap();

    assert!(
        !remote_account_provider.is_watching(&delegation_record_pubkey),
        "delegation record direct ref should not leak"
    );
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
    // This test validates that pending clone ownership prevents duplicate
    // clone submissions when the fetch and subscription paths race on the
    // same (pubkey, slot).
    //
    // The first caller to claim the (pubkey, slot) becomes the "owner" and
    // performs the actual clone. The second caller becomes a "waiter" and
    // receives the result via a oneshot channel without submitting a
    // duplicate clone transaction.

    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    // Non-delegated account so the subscription path goes through
    // resolve → clone_account_with_ownership() independently instead
    // of being intercepted by greedy clone.
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

    // Clone delay ensures both paths enter clone_account_with_ownership
    // before the owner finishes, so the second caller becomes a waiter.
    let cloner_stub = Arc::new(ClonerStub::new(accounts_bank.clone()));
    cloner_stub.set_clone_delay(std::time::Duration::from_millis(200));

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

    // Send subscription update (this will become the owner).
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

    // Let subscription listener pick up the update and start cloning.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Trigger concurrent fetch (becomes a waiter via pending_clones).
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

    let fetch_result = fetch_task.await.unwrap();
    assert!(
        fetch_result.is_ok(),
        "Fetch should succeed, got: {:?}",
        fetch_result
    );

    // Wait for subscription path to finish too.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Only one clone request should have been submitted.
    let same_account_clones = cloner_stub
        .clone_requests()
        .iter()
        .filter(|r| {
            r.pubkey == account_pubkey
                && r.account.remote_slot() == CURRENT_SLOT
        })
        .count();

    assert_eq!(
        same_account_clones, 1,
        "Expected 1 clone request (ownership should prevent duplicate)"
    );

    assert!(
        accounts_bank.get_account(&account_pubkey).is_some(),
        "Account should be present in bank"
    );
}

#[tokio::test]
async fn test_delegated_account_fetch_subscription_race() {
    // Validates that pending clone ownership also works for delegated
    // accounts. A DLP-owned delegated account sent via subscription
    // and concurrently fetched should produce exactly one clone request.

    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
    let account_owner = random_pubkey();

    // Create a DLP-owned account (delegation program owns it).
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: dlp_api::id(),
        executable: false,
        rent_epoch: 0,
    };

    let FetcherTestCtx {
        accounts_bank,
        remote_account_provider,
        rpc_client,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    // Add delegation record so the account resolves as delegated to us.
    add_delegation_record_with_slot_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
        CURRENT_SLOT,
    );

    let cloner_stub = Arc::new(ClonerStub::new(accounts_bank.clone()));
    cloner_stub.set_clone_delay(std::time::Duration::from_millis(200));

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

    // Send subscription update.
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

    tokio::time::sleep(Duration::from_millis(20)).await;

    // Trigger concurrent fetch.
    let fetch_task = {
        let fc = fetch_cloner.clone();
        tokio::spawn(async move {
            fc.fetch_and_clone_accounts_with_dedup(
                &[account_pubkey],
                None,
                Some(CURRENT_SLOT),
                AccountFetchOrigin::GetAccount,
                None,
            )
            .await
        })
    };

    let fetch_result = fetch_task.await.unwrap();
    assert!(
        fetch_result.is_ok(),
        "Fetch should succeed, got: {:?}",
        fetch_result
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let same_account_clones = cloner_stub
        .clone_requests()
        .iter()
        .filter(|r| {
            r.pubkey == account_pubkey
                && r.account.remote_slot() == CURRENT_SLOT
        })
        .count();

    assert_eq!(
        same_account_clones, 1,
        "Expected 1 clone request for delegated account"
    );

    assert!(
        accounts_bank.get_account(&account_pubkey).is_some(),
        "Delegated account should be present in bank"
    );
}

#[tokio::test]
async fn test_clone_ownership_failure_propagates_to_waiters() {
    // Validates that when the clone owner fails, waiters receive the
    // failure and don't hang. Also verifies that the pending entry is
    // cleared so a subsequent clone attempt can proceed.

    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkey = random_pubkey();
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

    let cloner_stub = Arc::new(ClonerStub::new(accounts_bank.clone()));
    cloner_stub.set_clone_delay(std::time::Duration::from_millis(200));
    // The first clone attempt will fail.
    cloner_stub.set_fail_next_clone(true);

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

    // Send subscription update (becomes owner, will fail).
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

    tokio::time::sleep(Duration::from_millis(20)).await;

    // Concurrent fetch (becomes waiter, should receive failure).
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

    // The fetch task should complete (not hang) even though
    // the owner failed.
    let fetch_result = tokio::time::timeout(Duration::from_secs(2), fetch_task)
        .await
        .expect("Fetch task should not hang when owner fails")
        .unwrap();

    // Fetch returns an error since the owner failed.
    assert!(
        fetch_result.is_err(),
        "Fetch should fail when clone owner fails"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Only one clone submission was attempted.
    assert_eq!(
        cloner_stub.clone_request_count(),
        1,
        "Only the owner should have submitted a clone request"
    );

    // Pending entry is cleared: a subsequent clone can proceed.
    let retry_result = fetch_cloner
        .fetch_and_clone_accounts_with_dedup(
            &[account_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await;
    assert!(
        retry_result.is_ok(),
        "Retry after failure should succeed, got: {:?}",
        retry_result
    );

    // Now account should be in bank (second clone succeeded).
    assert!(
        accounts_bank.get_account(&account_pubkey).is_some(),
        "Account should be present in bank after retry"
    );
}

#[tokio::test]
async fn test_ata_projection_releases_ata_direct_ref_after_fetch() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const AMOUNT: u64 = 777;

    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_account = create_ata_account(&wallet_owner, &mint);
    let eata_account = create_eata_account(&wallet_owner, &mint, AMOUNT, true);

    let FetcherTestCtx {
        remote_account_provider,
        fetch_cloner,
        rpc_client,
        ..
    } = setup(
        [(ata_pubkey, ata_account), (eata_pubkey, eata_account)],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_with_slot_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
        CURRENT_SLOT + 1,
    );

    let result = fetch_cloner
        .fetch_and_clone_accounts_with_dedup(
            &[ata_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await
        .expect("ATA projection fetch should not fail");
    assert!(result.is_ok(), "ATA projection fetch should succeed");

    assert!(
        !remote_account_provider.is_watching(&ata_pubkey),
        "ATA direct subscription must be released after projection fetch"
    );
    assert!(
        !remote_account_provider.is_watching(&eata_pubkey),
        "eATA direct/projection subscriptions must be released after projection fetch"
    );
}

#[tokio::test]
async fn test_fetch_keeps_undelegating_projected_ata_in_bank() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const LOCAL_SLOT: u64 = CURRENT_SLOT - 1;
    const CHAIN_EATA_AMOUNT: u64 = 777;
    const LOCAL_ATA_AMOUNT: u64 = 999;

    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_account = create_ata_account(&wallet_owner, &mint);
    let eata_account =
        create_eata_account(&wallet_owner, &mint, CHAIN_EATA_AMOUNT, true);

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        rpc_client,
        ..
    } = setup(
        [
            (ata_pubkey, ata_account.clone()),
            (eata_pubkey, eata_account),
        ],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_with_slot_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
        CURRENT_SLOT + 1,
    );

    let mut local_ata = create_ata_account(&wallet_owner, &mint);
    local_ata.data[64..72].copy_from_slice(&LOCAL_ATA_AMOUNT.to_le_bytes());
    let mut local_ata_shared = AccountSharedData::from(local_ata);
    local_ata_shared.set_owner(dlp_api::id());
    local_ata_shared.set_remote_slot(LOCAL_SLOT);
    local_ata_shared.set_undelegating(true);
    accounts_bank.insert(ata_pubkey, local_ata_shared);

    let result = fetch_cloner
        .fetch_and_clone_accounts_with_dedup(
            &[ata_pubkey],
            None,
            None,
            AccountFetchOrigin::GetAccount,
            None,
        )
        .await
        .expect("fetch should succeed");
    assert!(result.is_ok());

    let ata_after = accounts_bank
        .get_account(&ata_pubkey)
        .expect("ATA should still exist in bank");
    assert!(ata_after.undelegating(), "ATA must remain undelegating");
    assert_eq!(
        ata_after.remote_slot(),
        LOCAL_SLOT,
        "Undelegating ATA should keep its local slot",
    );
    assert_eq!(
        *ata_after.owner(),
        dlp_api::id(),
        "Undelegating ATA should remain locked to the delegation program",
    );
    let ata_amount =
        u64::from_le_bytes(ata_after.data()[64..72].try_into().unwrap());
    assert_eq!(
        ata_amount, LOCAL_ATA_AMOUNT,
        "Undelegating ATA amount should keep local state",
    );
}

#[tokio::test]
async fn test_undelegating_projected_ata_subscription_update_stays_locked() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const LOCAL_SLOT: u64 = CURRENT_SLOT - 1;
    const LOCAL_ATA_AMOUNT: u64 = 999;

    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let eata_pubkey = derive_eata(&wallet_owner, &mint);
    let ata_account = create_ata_account(&wallet_owner, &mint);
    let eata_account = create_eata_account(&wallet_owner, &mint, 777, true);

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        subscription_tx,
        ..
    } = setup(
        [
            (ata_pubkey, ata_account.clone()),
            (eata_pubkey, eata_account),
        ],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    add_delegation_record_with_slot_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
        CURRENT_SLOT + 1,
    );

    let mut local_ata = create_ata_account(&wallet_owner, &mint);
    local_ata.data[64..72].copy_from_slice(&LOCAL_ATA_AMOUNT.to_le_bytes());
    let mut local_ata_shared = AccountSharedData::from(local_ata);
    local_ata_shared.set_owner(dlp_api::id());
    local_ata_shared.set_remote_slot(LOCAL_SLOT);
    local_ata_shared.set_undelegating(true);
    accounts_bank.insert(ata_pubkey, local_ata_shared);
    assert_not_subscribed!(remote_account_provider, &[&eata_pubkey]);

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    subscription_tx
        .send(ForwardedSubscriptionUpdate {
            pubkey: ata_pubkey,
            account: RemoteAccount::from_fresh_account(
                ata_account,
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
            if remote_account_provider.is_watching(&eata_pubkey) {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for projected ATA subscription update");
    assert_subscribed!(remote_account_provider, &[&eata_pubkey]);

    let ata_after = accounts_bank
        .get_account(&ata_pubkey)
        .expect("ATA should still exist in bank");
    assert!(ata_after.undelegating(), "ATA must remain undelegating");
    assert_eq!(
        ata_after.remote_slot(),
        LOCAL_SLOT,
        "Undelegating ATA should keep its local slot",
    );
    assert_eq!(
        *ata_after.owner(),
        dlp_api::id(),
        "Undelegating ATA should remain locked to the delegation program",
    );
    let ata_amount =
        u64::from_le_bytes(ata_after.data()[64..72].try_into().unwrap());
    assert_eq!(
        ata_amount, LOCAL_ATA_AMOUNT,
        "Undelegating ATA amount should keep local state",
    );
}

#[tokio::test]
async fn test_delegated_eata_update_does_not_override_undelegating_ata_in_bank()
{
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const LOCAL_SLOT: u64 = CURRENT_SLOT - 1;
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

    add_delegation_record_with_slot_for(
        &rpc_client,
        eata_pubkey,
        validator_pubkey,
        EATA_PROGRAM_ID,
        CURRENT_SLOT + 1,
    );

    let mut local_ata = create_ata_account(&wallet_owner, &mint);
    local_ata.data[64..72].copy_from_slice(&LOCAL_ATA_AMOUNT.to_le_bytes());
    let mut local_ata_shared = AccountSharedData::from(local_ata);
    local_ata_shared.set_owner(dlp_api::id());
    local_ata_shared.set_remote_slot(LOCAL_SLOT);
    local_ata_shared.set_undelegating(true);
    accounts_bank.insert(ata_pubkey, local_ata_shared);

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
        while accounts_bank.get_account(&eata_pubkey).is_none() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for delegated eATA subscription update");

    let ata_after = accounts_bank
        .get_account(&ata_pubkey)
        .expect("ATA should still exist in bank");
    assert!(ata_after.undelegating(), "ATA must remain undelegating");
    assert_eq!(
        ata_after.remote_slot(),
        LOCAL_SLOT,
        "Undelegating ATA should keep its local slot",
    );
    assert_eq!(
        *ata_after.owner(),
        dlp_api::id(),
        "Undelegating ATA should remain locked to the delegation program",
    );
    let ata_amount =
        u64::from_le_bytes(ata_after.data()[64..72].try_into().unwrap());
    assert_eq!(
        ata_amount, LOCAL_ATA_AMOUNT,
        "Undelegating ATA amount should keep local state",
    );
}

#[tokio::test]
async fn test_owned_operation_concurrent_calls_spawn_one_owner_fetch() {
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

    let (result1, result2) =
        tokio::try_join!(spawn_fetch_task(), spawn_fetch_task()).unwrap();

    assert!(result1.is_ok(), "first concurrent fetch should succeed");
    assert!(result2.is_ok(), "second concurrent fetch should succeed");
    assert_eq!(fetch_cloner.fetch_count(), 1);
    assert_cloned_undelegated_account!(
        accounts_bank,
        account_pubkey,
        account,
        CURRENT_SLOT,
        account_owner
    );
}

#[tokio::test]
async fn test_owned_operation_waiters_share_not_found_metadata() {
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const INITIAL_ACC_SLOT: u64 = CURRENT_SLOT - 10;

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

    rpc_client.account_override_slot(&account_pubkey, INITIAL_ACC_SLOT);
    add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );

    let fetch_cloner = Arc::new(fetch_cloner);
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

    let (result1, result2) =
        tokio::try_join!(spawn_fetch_task(), spawn_fetch_task()).unwrap();
    let result1 = result1.expect("first concurrent fetch should succeed");
    let result2 = result2.expect("second concurrent fetch should succeed");

    assert_eq!(
        result1.not_found_on_chain,
        vec![(account_pubkey, CURRENT_SLOT)]
    );
    assert_eq!(
        result2.not_found_on_chain,
        vec![(account_pubkey, CURRENT_SLOT)]
    );
    assert!(result1.missing_delegation_record.is_empty());
    assert!(result2.missing_delegation_record.is_empty());
}

#[tokio::test]
async fn test_owned_operation_waiters_share_missing_delegation_record_metadata()
{
    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;
    const INITIAL_DELEG_RECORD_SLOT: u64 = CURRENT_SLOT - 10;

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

    let deleg_record_pubkey = add_delegation_record_for(
        &rpc_client,
        account_pubkey,
        validator_pubkey,
        account_owner,
    );
    rpc_client
        .account_override_slot(&deleg_record_pubkey, INITIAL_DELEG_RECORD_SLOT);

    let fetch_cloner = Arc::new(fetch_cloner);
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

    let (result1, result2) =
        tokio::try_join!(spawn_fetch_task(), spawn_fetch_task()).unwrap();
    let result1 = result1.expect("first concurrent fetch should succeed");
    let result2 = result2.expect("second concurrent fetch should succeed");

    assert_eq!(
        result1.missing_delegation_record,
        vec![(account_pubkey, CURRENT_SLOT)]
    );
    assert_eq!(
        result2.missing_delegation_record,
        vec![(account_pubkey, CURRENT_SLOT)]
    );
    assert!(result1.not_found_on_chain.is_empty());
    assert!(result2.not_found_on_chain.is_empty());
}

#[tokio::test]
async fn test_pending_deadline_is_not_extended_by_late_joiners() {
    let pending = Arc::new(scc::HashMap::<Pubkey, Pending>::new());
    let pubkey = random_pubkey();
    let owner_budget = Duration::from_millis(200);
    let joiner_budget = Duration::from_secs(10);

    let owner_handles = match claim_or_join_pending(
        pending.clone(),
        pubkey,
        1,
        1,
        owner_budget,
    ) {
        PendingClaim::Created(handles) => handles,
        PendingClaim::Joined(_) => panic!("expected owner to create pending"),
    };

    tokio::time::sleep(Duration::from_millis(20)).await;

    let joiner_handles = match claim_or_join_pending(
        pending.clone(),
        pubkey,
        2,
        2,
        joiner_budget,
    ) {
        PendingClaim::Joined(handles) => handles,
        PendingClaim::Created(_) => {
            panic!("expected late joiner to join pending")
        }
    };

    assert_eq!(owner_handles.deadline, joiner_handles.deadline);
    assert!(Arc::ptr_eq(&owner_handles.cancel, &joiner_handles.cancel));
    assert_eq!(
        joiner_handles.waiter.generation(),
        owner_handles.waiter.generation()
    );

    let generation = owner_handles.waiter.generation();
    let count = finish_pending(
        &pending,
        pubkey,
        generation,
        PendingTerminal::Failed(PendingFailure::TimedOut),
    );
    assert_eq!(count, 2);

    let owner_terminal = owner_handles
        .waiter
        .wait()
        .await
        .expect("owner waiter should receive terminal result");
    let joiner_terminal = joiner_handles
        .waiter
        .wait()
        .await
        .expect("joiner waiter should receive terminal result");

    assert!(matches!(
        owner_terminal,
        PendingTerminal::Failed(PendingFailure::TimedOut)
    ));
    assert!(matches!(
        joiner_terminal,
        PendingTerminal::Failed(PendingFailure::TimedOut)
    ));
}

#[tokio::test]
async fn test_owned_operation_waiter_cancellation_is_local() {
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
        rpc_client,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let fetch_cloner = Arc::new(fetch_cloner);
    rpc_client.block_fetches();

    let owner_task = {
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

    wait_for_pending_request(&fetch_cloner, account_pubkey).await;
    wait_for_rpc_fetch_activity(&rpc_client, 1).await;

    let waiter_task = {
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

    wait_for_pending_waiter_count(&fetch_cloner, account_pubkey, 2).await;
    waiter_task.abort();
    let _ = waiter_task.await;
    wait_for_pending_waiter_count(&fetch_cloner, account_pubkey, 1).await;

    rpc_client.allow_fetches();

    let owner_result =
        owner_task.await.expect("owner task join should succeed");
    assert!(
        owner_result.is_ok(),
        "owner fetch should complete successfully"
    );
    assert!(!fetch_cloner.has_pending_request(&account_pubkey));
    assert_cloned_undelegated_account!(
        accounts_bank,
        account_pubkey,
        account,
        CURRENT_SLOT,
        account_owner
    );
}

#[tokio::test]
async fn test_owned_operation_owner_timeout_cleans_up_pending() {
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
        [(account_pubkey, account)],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let blocking_cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
    blocking_cloner.block_clone_completion();

    let (_subscription_tx, subscription_rx) = mpsc::channel(100);
    let fetch_cloner = FetchCloner::new(
        &fetch_cloner.remote_account_provider.clone(),
        &accounts_bank,
        &blocking_cloner,
        validator_keypair.insecure_clone(),
        Pubkey::new_unique(),
        subscription_rx,
        None,
    );

    let owner_task = {
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

    wait_for_pending_request(&fetch_cloner, account_pubkey).await;
    let waiter_task = {
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

    wait_for_pending_waiter_count(&fetch_cloner, account_pubkey, 2).await;
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    while blocking_cloner.clone_request_count() < 1 && start.elapsed() < timeout
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(blocking_cloner.clone_request_count(), 1);

    let owner_result = tokio::time::timeout(
        FETCH_CLONE_OPERATION_TIMEOUT + Duration::from_secs(5),
        owner_task,
    )
    .await
    .expect("owner timeout test should complete within the outer timeout")
    .expect("owner task join should succeed");
    let waiter_result =
        tokio::time::timeout(Duration::from_secs(5), waiter_task)
            .await
            .expect("waiter should complete after owner timeout")
            .expect("waiter task join should succeed");
    assert!(matches!(
        owner_result,
        Err(ChainlinkError::PendingRequestTimeout(pubkey))
            if pubkey == account_pubkey
    ));
    assert!(matches!(
        waiter_result,
        Err(ChainlinkError::PendingRequestTimeout(pubkey))
            if pubkey == account_pubkey
    ));
    assert!(!fetch_cloner.has_pending_request(&account_pubkey));

    blocking_cloner.allow_clone_completion();
}

#[tokio::test]
async fn test_cancel_pending_terminates_owner_and_all_waiters() {
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
        [(account_pubkey, account)],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let blocking_cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
    blocking_cloner.block_clone_completion();

    let (_subscription_tx, subscription_rx) = mpsc::channel(100);
    let fetch_cloner = FetchCloner::new(
        &fetch_cloner.remote_account_provider.clone(),
        &accounts_bank,
        &blocking_cloner,
        validator_keypair.insecure_clone(),
        Pubkey::new_unique(),
        subscription_rx,
        None,
    );

    let owner_task = {
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

    wait_for_pending_request(&fetch_cloner, account_pubkey).await;
    let waiter_task_a = {
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
    let waiter_task_b = {
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

    wait_for_pending_waiter_count(&fetch_cloner, account_pubkey, 3).await;
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    while blocking_cloner.clone_request_count() < 1 && start.elapsed() < timeout
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(blocking_cloner.clone_request_count(), 1);

    fetch_cloner.cancel_pending(&account_pubkey);

    let owner_result = tokio::time::timeout(Duration::from_secs(5), owner_task)
        .await
        .expect("owner should complete after pending cancellation")
        .expect("owner task join should succeed");
    let waiter_result_a =
        tokio::time::timeout(Duration::from_secs(5), waiter_task_a)
            .await
            .expect("first waiter should complete after pending cancellation")
            .expect("first waiter task join should succeed");
    let waiter_result_b =
        tokio::time::timeout(Duration::from_secs(5), waiter_task_b)
            .await
            .expect("second waiter should complete after pending cancellation")
            .expect("second waiter task join should succeed");

    for result in [owner_result, waiter_result_a, waiter_result_b] {
        assert!(matches!(
            result,
            Err(ChainlinkError::PendingRequestCancelled(pubkey))
                if pubkey == account_pubkey
        ));
    }
    assert!(!fetch_cloner.has_pending_request(&account_pubkey));

    blocking_cloner.allow_clone_completion();
}

#[tokio::test]
async fn test_cancel_all_pending_on_shutdown() {
    init_logger();
    let validator_keypair = Keypair::new();
    let account_owner = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let account_pubkeys = [random_pubkey(), random_pubkey(), random_pubkey()];
    let accounts = account_pubkeys.map(|pubkey| {
        (
            pubkey,
            Account {
                lamports: 2_000_000,
                data: vec![5, 6, 7, 8],
                owner: account_owner,
                executable: false,
                rent_epoch: 0,
            },
        )
    });

    let FetcherTestCtx {
        accounts_bank,
        fetch_cloner,
        ..
    } = setup(accounts, CURRENT_SLOT, validator_keypair.insecure_clone()).await;

    let blocking_cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
    blocking_cloner.block_clone_completion();

    let (_subscription_tx, subscription_rx) = mpsc::channel(100);
    let fetch_cloner = FetchCloner::new(
        &fetch_cloner.remote_account_provider.clone(),
        &accounts_bank,
        &blocking_cloner,
        validator_keypair.insecure_clone(),
        Pubkey::new_unique(),
        subscription_rx,
        None,
    );

    let mut tasks = Vec::new();
    for pubkey in account_pubkeys {
        let fetch_cloner = fetch_cloner.clone();
        tasks.push((
            pubkey,
            tokio::spawn(async move {
                fetch_cloner
                    .fetch_and_clone_accounts_with_dedup(
                        &[pubkey],
                        None,
                        None,
                        AccountFetchOrigin::GetAccount,
                        None,
                    )
                    .await
            }),
        ));
    }

    for pubkey in account_pubkeys {
        wait_for_pending_request(&fetch_cloner, pubkey).await;
    }

    for pubkey in account_pubkeys {
        let fetch_cloner = fetch_cloner.clone();
        tasks.push((
            pubkey,
            tokio::spawn(async move {
                fetch_cloner
                    .fetch_and_clone_accounts_with_dedup(
                        &[pubkey],
                        None,
                        None,
                        AccountFetchOrigin::GetAccount,
                        None,
                    )
                    .await
            }),
        ));
    }

    for pubkey in account_pubkeys {
        wait_for_pending_waiter_count(&fetch_cloner, pubkey, 2).await;
    }

    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    while blocking_cloner.clone_request_count() < account_pubkeys.len()
        && start.elapsed() < timeout
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(blocking_cloner.clone_request_count(), account_pubkeys.len());

    fetch_cloner.cancel_all_pending();

    for (expected_pubkey, task) in tasks {
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("task should complete after bulk pending cancellation")
            .expect("task join should succeed");
        assert!(matches!(
            result,
            Err(ChainlinkError::PendingRequestCancelled(pubkey))
                if pubkey == expected_pubkey && account_pubkeys.contains(&pubkey)
        ));
    }

    for pubkey in account_pubkeys {
        assert!(!fetch_cloner.has_pending_request(&pubkey));
    }

    blocking_cloner.allow_clone_completion();
}

#[tokio::test]
async fn test_owned_operation_waiters_do_not_refetch_after_owner_success() {
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
        rpc_client,
        ..
    } = setup(
        [(account_pubkey, account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;

    let fetch_cloner = Arc::new(fetch_cloner);
    rpc_client.block_fetches();

    let owner_task = {
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

    wait_for_pending_request(&fetch_cloner, account_pubkey).await;
    wait_for_rpc_fetch_activity(&rpc_client, 1).await;

    let waiter_task = {
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

    wait_for_pending_waiter_count(&fetch_cloner, account_pubkey, 2).await;
    rpc_client.allow_fetches();

    let owner_result =
        owner_task.await.expect("owner task join should succeed");
    let waiter_result =
        waiter_task.await.expect("waiter task join should succeed");

    assert!(owner_result.is_ok(), "owner fetch should succeed");
    assert!(waiter_result.is_ok(), "waiter fetch should succeed");
    assert_eq!(fetch_cloner.fetch_count(), 1);
    assert!(
        rpc_client.single_account_fetches()
            + rpc_client.multi_account_fetches()
            >= 1
    );
    assert_cloned_undelegated_account!(
        accounts_bank,
        account_pubkey,
        account,
        CURRENT_SLOT,
        account_owner
    );
}

#[tokio::test]
async fn test_project_ata_skips_repeat_fetch_for_known_empty_eata() {
    init_logger();
    let validator_keypair = Keypair::new();
    let wallet_owner = random_pubkey();
    let mint = random_pubkey();
    const CURRENT_SLOT: u64 = 100;

    let ata_pubkey = derive_ata(&wallet_owner, &mint);
    let ata_account = create_ata_account(&wallet_owner, &mint);

    let FetcherTestCtx {
        remote_account_provider,
        accounts_bank,
        rpc_client,
        subscription_tx,
        fetch_cloner,
        ..
    } = setup(
        [(ata_pubkey, ata_account.clone())],
        CURRENT_SLOT,
        validator_keypair.insecure_clone(),
    )
    .await;
    let eata_pubkey = derive_eata(&wallet_owner, &mint);

    use crate::remote_account_provider::{
        RemoteAccount, RemoteAccountUpdateSource,
    };

    let send_update = |slot| {
        let ata_account = ata_account.clone();
        let subscription_tx = subscription_tx.clone();
        async move {
            subscription_tx
                .send(ForwardedSubscriptionUpdate {
                    pubkey: ata_pubkey,
                    account: RemoteAccount::from_fresh_account(
                        ata_account,
                        slot,
                        RemoteAccountUpdateSource::Subscription,
                    ),
                })
                .await
                .unwrap();
        }
    };

    const POLL_INTERVAL: std::time::Duration = Duration::from_millis(10);
    const TIMEOUT: std::time::Duration = Duration::from_millis(500);

    let baseline_fetches = rpc_client.single_account_fetches();
    send_update(CURRENT_SLOT).await;
    tokio::time::timeout(TIMEOUT, async {
        while !fetch_cloner.is_known_empty_eata(&eata_pubkey) {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for known-empty eATA cache entry");
    let after_first = rpc_client.single_account_fetches();
    assert!(
        after_first > baseline_fetches,
        "first ATA update should trigger an upstream eATA fetch"
    );

    assert!(
        remote_account_provider.is_watching(&eata_pubkey),
        "eATA must be subscribed after the first ATA update"
    );

    const SECOND_SLOT: u64 = CURRENT_SLOT + 1;
    send_update(SECOND_SLOT).await;
    tokio::time::timeout(TIMEOUT, async {
        while accounts_bank
            .get_account(&ata_pubkey)
            .is_none_or(|account| account.remote_slot() < SECOND_SLOT)
        {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for banked ATA subscription update");
    assert_eq!(
        rpc_client.single_account_fetches(),
        after_first,
        "subsequent ATA updates for an already-known-empty eATA must not refetch"
    );
    assert!(
        remote_account_provider.is_watching(&eata_pubkey),
        "eATA subscription must persist across cache-hit ATA updates"
    );
}

#[tokio::test]
async fn test_delegated_account_owned_by_token_program_does_not_subscribe_program(
) {
    use magicblock_core::token_programs::TOKEN_PROGRAM_ID;

    init_logger();
    let validator_keypair = Keypair::new();
    let validator_pubkey = validator_keypair.pubkey();
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
        TOKEN_PROGRAM_ID,
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
    assert!(result.is_ok());

    tokio::time::sleep(Duration::from_millis(100)).await;

    let pubsub_client = remote_account_provider.pubsub_client();
    let subscribed_programs = pubsub_client.subscribed_program_ids();
    assert!(
        !subscribed_programs.contains(&TOKEN_PROGRAM_ID),
        "must never subscribe to SPL Token program (owns too many accounts), got: {:?}",
        subscribed_programs
    );
}
