#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Redundant source loss, cache eviction, and reacquisition.

mod support;

use std::{collections::HashMap, slice::from_ref};

use chainsync::{AccountProperty, ChainSyncAccount};
use engine::testkit::{Pacing, TestEngine};
use keeper::testkit::{Dirs, keeper_builder};
use magicblock_chainsync as chainsync;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::clock::ID as CLOCK_ID;
use support::yellowstone::wait_for_retained_update;
use support::{
    Account, CONTEXT_SLOT, DUPLICATION_DELAY, FIRST_SUBSCRIPTION, READY_SLOT, RPC_BATCH_LIMIT,
    START_SLOT, acquire, config, http, scripted, start, websocket, within, yellowstone,
};

const CACHE_CAPACITY: usize = 256;
const REACQUIRED_SUBSCRIPTION: u64 = 1000;
const STALE_DATA: u8 = 99;
const BEFORE_LOSS_DATA: u8 = 2;
const AFTER_LOSS_DATA: u8 = 3;
const REFETCHED_DATA: u8 = 4;
const WS_DATA: u8 = 5;
const REACQUIRED_DATA: u8 = 8;
const REPLACEMENT_SUBSCRIPTION: u64 = FIRST_SUBSCRIPTION + 1;

/// gRPC survives WS loss; last-source loss deletes the mirror, and the next sync subscribes and fetches anew.
#[tokio::test]
async fn grpc_survives_websocket_loss_then_full_loss_requires_reacquisition() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let clock = Account::new(CLOCK_ID);
    grpc.seed(READY_SLOT, from_ref(&clock)).await;
    engine.accounts().advance_chain_slot(START_SLOT);
    let mut config = config(&http.endpoint, Some(&ws), &[&grpc.endpoint]);
    for stream in &mut config.grpc {
        stream.duplication_delay = DUPLICATION_DELAY;
    }
    let chain_sync = start(&mut engine, config);
    within(async {
        while engine.accounts().chain_slot() < READY_SLOT {
            engine.advance(1).await;
        }
    })
    .await;
    ws.wait_for_connection().await;
    let key = Pubkey::new_unique();
    let accounts = [ChainSyncAccount {
        pubkey: key,
        property: AccountProperty::Readonly,
    }];
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION);
        http.next().await.respond_accounts(CONTEXT_SLOT, &[Some(Account::new(key))]);
    };
    scripted(acquire(&chain_sync, &accounts), script).await.unwrap();
    let mut updates = engine.accounts().subscribe(key);
    let mut grpc_image = Account::new(key);
    let mut slot = CONTEXT_SLOT;
    for data in [BEFORE_LOSS_DATA, AFTER_LOSS_DATA] {
        if data == AFTER_LOSS_DATA {
            ws.disconnect();
        }
        grpc_image.data = vec![data];
        // Neither image is sent over WS: both materializations prove gRPC coverage.
        let delivery = wait_for_retained_update(&mut engine, from_ref(&grpc), &grpc_image).await;
        slot = delivery.sent_slot;
        assert_eq!(delivery.account.data(), &[data]);
    }
    assert!(engine.get_account(key).is_some());
    let mut processed = engine.transactions().subscribe_processed().unwrap();
    // A fatal, filtered transaction ends the outer session and emits Lost without a reconnect delay.
    grpc.malformed_transaction(slot + 1);
    within(async {
        loop {
            processed.recv().await.unwrap();
            if engine.get_account(key).is_none() {
                break;
            }
        }
    })
    .await;
    assert!(engine.get_account(key).is_none());
    ws.wait_for_connection().await;
    engine.advance(1).await;
    let refetch_slot = slot + 2;
    let script = async {
        let call = ws.next().await;
        assert_eq!(call.pubkey, key);
        assert!(
            !http.pending(),
            "reacquisition still subscribes before fetching"
        );
        call.ack(REPLACEMENT_SUBSCRIPTION);
        let call = http.next().await;
        assert!(call.min_context_slot().unwrap() >= slot);
        let mut image = Account::new(key);
        image.data = vec![REFETCHED_DATA];
        call.respond_accounts(refetch_slot, &[Some(image)]);
    };
    scripted(acquire(&chain_sync, &accounts), script).await.unwrap();
    assert_eq!(engine.get_account(key).unwrap().data(), &[REFETCHED_DATA]);
    let mut image = Account::new(key);
    image.data = vec![STALE_DATA];
    ws.notify(FIRST_SUBSCRIPTION, refetch_slot + 2, &image);
    image.data = vec![WS_DATA];
    ws.notify(REPLACEMENT_SUBSCRIPTION, refetch_slot + 1, &image);
    within(async {
        loop {
            let update = updates.recv().await.unwrap();
            assert_ne!(
                update.lamports(),
                0,
                "an old ID cannot evict the replacement"
            );
            if update.slot() > refetch_slot {
                break;
            }
        }
    })
    .await;
    assert_eq!(engine.get_account(key).unwrap().data(), &[WS_DATA]);
    assert_eq!(engine.get_account(key).unwrap().slot(), refetch_slot + 1);
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// An actually established gRPC copy can fail without interrupting the remaining WS subscription.
#[tokio::test]
async fn websocket_updates_continue_after_established_grpc_loss() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    engine.accounts().advance_chain_slot(START_SLOT);
    let mut config = config(&http.endpoint, Some(&ws), &[&grpc.endpoint]);
    for stream in &mut config.grpc {
        stream.duplication_delay = DUPLICATION_DELAY;
    }
    let chain_sync = start(&mut engine, config);
    ws.wait_for_connection().await;
    let key = Pubkey::new_unique();
    let accounts = [ChainSyncAccount {
        pubkey: key,
        property: AccountProperty::Readonly,
    }];
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION);
        http.next().await.respond_accounts(CONTEXT_SLOT, &[Some(Account::new(key))]);
    };
    scripted(acquire(&chain_sync, &accounts), script).await.unwrap();
    let mut image = Account::new(key);
    image.data = vec![BEFORE_LOSS_DATA];
    let slot = wait_for_retained_update(&mut engine, from_ref(&grpc), &image).await.sent_slot;
    let mut updates = engine.accounts().subscribe(key);
    grpc.malformed_transaction(slot + 1);
    // Lost is queued before the stream terminates; observe the actual service
    // termination rather than inferring loss from a closed socket.
    within(engine.shutdown().wait()).await;
    image.data = vec![AFTER_LOSS_DATA];
    ws.notify(FIRST_SUBSCRIPTION, slot + 2, &image);
    let accepted = within(async {
        loop {
            let update = updates.recv().await.unwrap();
            assert_ne!(
                update.lamports(),
                0,
                "partial loss cannot delete the mirror"
            );
            if update.slot() > slot {
                break update;
            }
        }
    })
    .await;
    assert_eq!(accepted.data(), &[AFTER_LOSS_DATA]);
    assert_eq!(engine.get_account(key).unwrap(), accepted);
    assert!(!http.pending(), "partial loss must not force a refetch");
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// Real recency-cache pressure removes a mirror and its subscription; reacquisition rejects buffered old IDs.
#[tokio::test]
async fn cache_eviction_unsubscribes_and_reacquisition_ignores_retired_ids() {
    let dirs = Dirs::default();
    let mut builder = keeper_builder(&dirs);
    builder.accountsdb.lru_capacity = CACHE_CAPACITY;
    let mut engine = TestEngine::from_builder(dirs, builder, Pacing::External).await;
    engine.accounts().advance_chain_slot(START_SLOT);
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let mut config = config(&http.endpoint, Some(&ws), &[&grpc.endpoint]);
    config.websocket.0[0].subs_per_connection = 512;
    let chain_sync = start(&mut engine, config);
    ws.wait_for_connection().await;
    // The Engine cache has a minimum capacity of 256. One extra materialization
    // guarantees pressure without testing which bucket or key its policy selects.
    let accounts: Vec<_> = (0..=CACHE_CAPACITY)
        .map(|_| ChainSyncAccount {
            pubkey: Pubkey::new_unique(),
            property: AccountProperty::Readonly,
        })
        .collect();
    let mut subscriptions = HashMap::new();
    let script = async {
        let mut id = 0;
        for batch in accounts.chunks(RPC_BATCH_LIMIT) {
            for _ in batch {
                let call = ws.next().await;
                let key = call.pubkey;
                id += 1;
                subscriptions.insert(id, key);
                call.ack(id);
            }
            let call = http.next().await;
            let images: Vec<_> = call.keys().map(|key| Some(Account::new(key))).collect();
            call.respond_accounts(CONTEXT_SLOT, &images);
        }
    };
    scripted(acquire(&chain_sync, &accounts), script).await.unwrap();
    let old_id = ws.unsubscribed().await;
    let key = subscriptions[&old_id];
    // The provider sees the release before Engine deletion finishes; reacquire
    // its lease to observe definitive completion rather than assuming an ACK is deletion.
    assert!(!within(engine.account(key)).await.unwrap().exists());
    engine.advance(1).await;
    let script = async {
        let call = ws.next().await;
        assert_eq!(call.pubkey, key);
        assert!(!http.pending());
        call.ack(REACQUIRED_SUBSCRIPTION);
        let mut image = Account::new(key);
        image.data = vec![7];
        http.next().await.respond_accounts(CONTEXT_SLOT + 1, &[Some(image)]);
    };
    scripted(
        acquire(
            &chain_sync,
            &[ChainSyncAccount {
                pubkey: key,
                property: AccountProperty::Readonly,
            }],
        ),
        script,
    )
    .await
    .unwrap();
    let mut updates = engine.accounts().subscribe(key);
    let mut image = Account::new(key);
    image.data = vec![STALE_DATA];
    ws.notify(old_id, CONTEXT_SLOT + 4, &image);
    image.data = vec![REACQUIRED_DATA];
    ws.notify(REACQUIRED_SUBSCRIPTION, CONTEXT_SLOT + 2, &image);
    within(async {
        loop {
            let update = updates.recv().await.unwrap();
            assert_ne!(
                update.lamports(),
                0,
                "an old ID cannot evict the reacquired mirror"
            );
            if update.slot() >= CONTEXT_SLOT + 2 {
                assert_eq!(update.data(), &[REACQUIRED_DATA]);
                break;
            }
        }
    })
    .await;
    assert_eq!(engine.get_account(key).unwrap().slot(), CONTEXT_SLOT + 2);
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}
