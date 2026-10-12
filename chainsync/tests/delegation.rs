#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Delegation ownership, replay, duplicate actions, and undelegation.

mod transport;

use std::slice::from_ref;

use chainsync::{AccountProperty, ChainSyncAccount};
use engine::testkit::TestEngine;
use keeper::testkit::store_v42;
use magicblock_chainsync as chainsync;
use nucleus::testkit::V42_ID;
use solana_account::{AccountMode, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::clock::ID as CLOCK_ID;
use transport::yellowstone::wait_for_retained_update;
use transport::{
    ACTION_AMOUNT, Account, CONTEXT_SLOT, DELEGATION_SLOT, DUPLICATION_DELAY, READY_SLOT,
    START_SLOT, V42_INITIAL, acquire, config, delegation, http, scripted, start, websocket, within,
    yellowstone,
};
use v42_calculator_interface::builder::transfer;

const ACCOUNT_SUBSCRIPTION: u64 = 41;
const CANARY_SUBSCRIPTION: u64 = 42;
const LOCAL_TRANSFER: i64 = 7;
const RELEASE_SLOT: u64 = DELEGATION_SLOT + 4;
const DUPLICATE_RELEASE_SLOT: u64 = RELEASE_SLOT + 2;

/// Opening replays two slots behind the persisted watermark; Clock continues advancing without tracked mirrors.
#[tokio::test]
async fn opening_replays_two_slots_back_and_clock_advances_without_tracking() {
    let mut engine = TestEngine::new().await;
    let http = http::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let key = Pubkey::new_unique();
    let (account, record) = delegation(key, V42_ID, engine.authority(), DELEGATION_SLOT, &[]);
    grpc.seed(DELEGATION_SLOT, &[account, record]).await;
    grpc.seed(DELEGATION_SLOT + 1, &[]).await;
    engine.accounts().advance_chain_slot(DELEGATION_SLOT + 2);
    let mut updates = engine.accounts().subscribe(key);
    let config = config(&http.endpoint, None, &[&grpc.endpoint]);
    let chain_sync = start(&mut engine, config);
    let account = within(updates.recv()).await.unwrap();
    assert_eq!(account.mode(), AccountMode::Delegated);
    assert_eq!(account.slot(), DELEGATION_SLOT);
    assert_eq!(account.owner(), &V42_ID);
    assert_eq!(engine.accounts().chain_slot(), DELEGATION_SLOT + 2);

    let clock = Account::new(CLOCK_ID);
    for slot in [100, 200] {
        within(async {
            while engine.accounts().chain_slot() < slot {
                grpc.confirmed(slot, from_ref(&clock));
                engine.advance(1).await;
            }
        })
        .await;
        assert_eq!(engine.accounts().chain_slot(), slot);
    }
    drop(chain_sync);
    engine.close().await;
    http.close().await;
    grpc.close().await;
}

/// Requests schedule once; releases restore live mirrors, and duplicate confirmations discover redelegation through HTTP waves.
#[tokio::test]
async fn grpc_undelegation_requests_and_confirmations_apply_once_across_streams() {
    use transport::{magic, undelegation_request};
    let mut engine = magic::engine().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let providers = yellowstone::Server::pair().await;
    engine.accounts().advance_chain_slot(START_SLOT);
    let config = config(
        &http.endpoint,
        Some(&ws),
        &[&providers[0].endpoint, &providers[1].endpoint],
    );
    let chain_sync = start(&mut engine, config);
    ws.wait_for_connection().await;
    for provider in &providers {
        provider.delegation_barrier(&engine, READY_SLOT).await;
    }
    let key = Pubkey::new_unique();
    let (account, record) = delegation(key, V42_ID, engine.authority(), DELEGATION_SLOT, &[]);
    let script = async {
        http.next()
            .await
            .respond_accounts(DELEGATION_SLOT, &[Some(account), Some(record)]);
    };
    scripted(
        chain_sync.ensure([ChainSyncAccount {
            pubkey: key,
            property: AccountProperty::Writable,
        }]),
        script,
    )
    .await
    .unwrap();
    assert_eq!(
        engine.get_account(key).unwrap().mode(),
        AccountMode::Delegated
    );
    let request = undelegation_request(key);
    for (provider, slot) in providers.iter().zip([DELEGATION_SLOT, DELEGATION_SLOT + 2]) {
        provider.confirmed(slot, from_ref(&request));
        provider.delegation_barrier(&engine, slot + 1).await;
        assert_eq!(
            engine.get_account(key).unwrap().mode(),
            AccountMode::Transient
        );
        assert_eq!(magic::intents(&engine), 1);
    }
    providers[0].undelegated(RELEASE_SLOT, key, engine.authority());
    let mut image = Account::new(key);
    image.owner = V42_ID;
    image.data = V42_INITIAL.to_le_bytes().to_vec();
    let script = async {
        let call = ws.next().await;
        assert_eq!(call.pubkey, key);
        call.ack(ACCOUNT_SUBSCRIPTION);
        let call = http.next().await;
        assert_eq!(call.min_context_slot(), Some(RELEASE_SLOT));
        assert_eq!(
            engine.get_account(key).unwrap().mode(),
            AccountMode::Transient
        );
        call.respond_accounts(RELEASE_SLOT, &[Some(image.clone())]);
    };
    scripted(
        providers[0].delegation_barrier(&engine, RELEASE_SLOT + 1),
        script,
    )
    .await;
    assert_eq!(
        engine.get_account(key).unwrap().mode(),
        AccountMode::ReadOnly
    );
    let mut updates = engine.accounts().subscribe(key);
    image.data = LOCAL_TRANSFER.to_le_bytes().to_vec();
    ws.notify(ACCOUNT_SUBSCRIPTION, RELEASE_SLOT + 1, &image);
    let updated = within(updates.recv()).await.unwrap();
    assert_eq!(updated.data(), &LOCAL_TRANSFER.to_le_bytes());
    assert_eq!(updated.mode(), AccountMode::ReadOnly);

    providers[1].undelegated(DUPLICATE_RELEASE_SLOT, key, engine.authority());
    let (account, record) =
        delegation(key, V42_ID, engine.authority(), DUPLICATE_RELEASE_SLOT, &[]);
    let script = async {
        // Readonly discovery reuses the active subscription, then acquires the delegation
        // record in the normal second wave without first deleting the resident mirror.
        let call = http.next().await;
        assert_eq!(call.keys().collect::<Vec<_>>(), [key]);
        assert_eq!(call.min_context_slot(), Some(DUPLICATE_RELEASE_SLOT));
        call.respond_accounts(DUPLICATE_RELEASE_SLOT, &[Some(account.clone())]);
        let call = http.next().await;
        assert_eq!(call.keys().collect::<Vec<_>>(), vec![key, record.key]);
        assert_eq!(call.min_context_slot(), Some(DUPLICATE_RELEASE_SLOT));
        call.respond_accounts(DUPLICATE_RELEASE_SLOT, &[Some(account), Some(record)]);
    };
    scripted(
        providers[1].delegation_barrier(&engine, DUPLICATE_RELEASE_SLOT + 1),
        script,
    )
    .await;
    let redelegated = engine.get_account(key).unwrap();
    assert_eq!(redelegated.mode(), AccountMode::Delegated);
    assert_eq!(redelegated.slot(), DUPLICATE_RELEASE_SLOT);
    // A duplicate refresh must not retain an extra reference that prevents unsubscribe.
    assert_eq!(ws.unsubscribed().await, ACCOUNT_SUBSCRIPTION);
    assert_eq!(magic::intents(&engine), 1);
    assert!(!http.pending());
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    for provider in providers {
        provider.close().await;
    }
}
/// Delegation removes both mirror subscriptions; replay from either provider cannot overwrite local execution or repeat actions.
#[tokio::test]
async fn grpc_delegation_retires_tracking_and_duplicates_preserve_local_execution() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let providers = yellowstone::Server::pair().await;
    engine.accounts().advance_chain_slot(START_SLOT);
    let mut config = config(
        &http.endpoint,
        Some(&ws),
        &[&providers[0].endpoint, &providers[1].endpoint],
    );
    for stream in &mut config.grpc {
        stream.duplication_delay = DUPLICATION_DELAY;
    }
    let chain_sync = start(&mut engine, config);
    ws.wait_for_connection().await;
    for provider in &providers {
        provider.delegation_barrier(&engine, READY_SLOT).await;
    }
    let key = Pubkey::new_unique();
    let canary = Pubkey::new_unique();
    let recipient = store_v42(&engine, 0, AccountMode::Delegated);
    let requests = [key, canary].map(|pubkey| ChainSyncAccount {
        pubkey,
        property: AccountProperty::Readonly,
    });
    let script = async {
        for _ in &requests {
            let call = ws.next().await;
            let id = if call.pubkey == key { ACCOUNT_SUBSCRIPTION } else { CANARY_SUBSCRIPTION };
            call.ack(id);
        }
        http.next().await.respond_accounts(
            CONTEXT_SLOT,
            &[Some(Account::new(key)), Some(Account::new(canary))],
        );
    };
    scripted(acquire(&chain_sync, &requests), script).await.unwrap();
    // Only the assigned stream retains this mirror; actual delivery establishes
    // coverage without coupling this test to the private assignment algorithm.
    let delivery = wait_for_retained_update(&mut engine, &providers, &Account::new(key)).await;
    let slot = delivery.sent_slot + 1;
    let (account, record) = delegation(
        key,
        V42_ID,
        engine.authority(),
        slot,
        &[transfer(key, recipient, ACTION_AMOUNT)],
    );
    providers[0].confirmed(slot, &[account.clone(), record.clone()]);
    providers[0].delegation_barrier(&engine, slot + 1).await;
    assert_eq!(ws.unsubscribed().await, ACCOUNT_SUBSCRIPTION);
    let delegated = engine.get_account(key).unwrap();
    assert_eq!(delegated.mode(), AccountMode::Delegated);
    assert_eq!(delegated.slot(), slot);
    assert_eq!(delegated.owner(), &V42_ID);
    assert_eq!(
        delegated.data(),
        (V42_INITIAL - ACTION_AMOUNT).to_le_bytes()
    );
    assert_eq!(
        engine.get_account(recipient).unwrap().data(),
        ACTION_AMOUNT.to_le_bytes()
    );
    within(engine.execute(&[transfer(key, recipient, LOCAL_TRANSFER)]))
        .await
        .unwrap();
    let local_sender = engine.get_account(key).unwrap();
    let local_recipient = engine.get_account(recipient).unwrap();

    let mut stale = Account::new(key);
    stale.owner = V42_ID;
    stale.data = 999i64.to_le_bytes().to_vec();
    let mut canaries = engine.accounts().subscribe(canary);
    // This notification is newer than the delegated image but belongs to a retired
    // provider ID. A live canary also proves the late ID did not drop the socket.
    ws.notify(ACCOUNT_SUBSCRIPTION, slot + 2, &stale);
    ws.notify(CANARY_SUBSCRIPTION, slot + 2, &Account::new(canary));
    within(async { while canaries.recv().await.unwrap().slot() != slot + 2 {} }).await;
    assert_eq!(engine.get_account(key).unwrap(), local_sender);
    for (index, provider) in providers.iter().enumerate() {
        // Ordinary mirror updates cannot overwrite delegated state, even at a newer slot.
        provider.confirmed(slot + 3, from_ref(&stale));
        provider.delegation_barrier(&engine, slot + 4).await;
        assert_eq!(engine.get_account(key).unwrap(), local_sender);
        // Replaying the delegation must neither restore its old balance nor rerun
        // the transfer action; check both sides against the later local execution.
        provider.confirmed(slot, &[account.clone(), record.clone()]);
        provider.delegation_barrier(&engine, slot + 5 + index as u64).await;
        assert_eq!(engine.get_account(key).unwrap(), local_sender);
        assert_eq!(engine.get_account(recipient).unwrap(), local_recipient);
    }
    assert_eq!(
        local_sender.data(),
        (V42_INITIAL - ACTION_AMOUNT - LOCAL_TRANSFER).to_le_bytes()
    );
    assert_eq!(
        local_recipient.data(),
        (ACTION_AMOUNT + LOCAL_TRANSFER).to_le_bytes()
    );
    assert!(!http.pending());
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    for provider in providers {
        provider.close().await;
    }
}
