use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    testing::{deleg::add_delegation_record_for, init_logger},
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;
use utils::accounts::account_shared_with_owner_and_slot;
use utils::test_context::TestContext;
mod utils;

async fn setup(slot: Slot) -> TestContext {
    init_logger();
    TestContext::init(slot).await
}

#[tokio::test]
async fn test_remote_slot_of_accounts_read_from_bank() {
    // This test ensures that the remote slot of accounts stored in the bank
    // is correctly included when we ensure read
    // It also ensures that we don't fetch accounts that are already in the bank
    // when ensuring reads
    let slot: u64 = 11;

    let ctx = setup(slot).await;
    let TestContext {
        chainlink,
        cloner,
        rpc_client,
        ..
    } = ctx.clone();

    // Setup chain to hold our account
    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let acc = Account {
        lamports: 1_000,
        ..Default::default()
    };
    let acc = account_shared_with_owner_and_slot(&acc, owner, slot);
    rpc_client.add_account(pubkey, acc.clone().into());

    assert_eq!(chainlink.fetch_count().unwrap(), 0);

    // 1. Read account first time which fetches it from chain
    chainlink.ensure_accounts(&[pubkey]).await.unwrap();
    assert_cloned_as_undelegated!(cloner, &[pubkey], slot, owner);
    assert_eq!(chainlink.fetch_count().unwrap(), 1);

    // 2. Read account again which gets it from bank (without fetching again)
    chainlink.ensure_accounts(&[pubkey]).await.unwrap();
    assert_cloned_as_undelegated!(cloner, &[pubkey], slot, owner);
    assert_eq!(chainlink.fetch_count().unwrap(), 1);
}

#[tokio::test]
async fn test_remote_slot_of_ensure_accounts_from_bank() {
    // This test ensures that the remote slot of accounts stored in the bank
    // is correctly included when we ensure write
    // It also ensures that we don't fetch accounts that are already in the bank
    // when ensuring writes
    let slot: u64 = 11;

    let ctx = setup(slot).await;
    let TestContext {
        chainlink,
        cloner,
        rpc_client,
        ..
    } = ctx.clone();

    // Setup chain to hold our delegated account
    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let acc = Account {
        lamports: 1_000,
        ..Default::default()
    };
    let delegated_acc =
        account_shared_with_owner_and_slot(&acc, dlp::id(), slot);
    rpc_client.add_account(pubkey, delegated_acc.into());
    add_delegation_record_for(&rpc_client, pubkey, ctx.validator_pubkey, owner);

    assert_eq!(chainlink.fetch_count().unwrap(), 0);

    // 1. Ensure account first time which fetches it from chain
    chainlink.ensure_accounts(&[pubkey]).await.unwrap();
    assert_cloned_as_delegated!(cloner, &[pubkey], slot, owner);

    // We fetch the account once then realize it is owned by the delegation record.
    // Then we fetch both again to ensure same slot
    assert_eq!(chainlink.fetch_count().unwrap(), 3);

    // 2. Ensure account again which gets it from bank (without fetching again)
    chainlink.ensure_accounts(&[pubkey]).await.unwrap();
    assert_cloned_as_delegated!(cloner, &[pubkey], slot, owner);
    // Since the account is already in the bank, we don't fetch it again
    assert_eq!(chainlink.fetch_count().unwrap(), 3);
}
