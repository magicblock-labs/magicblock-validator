#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! HTTP materialization, subscription ordering, and failed acquisition cleanup.

mod support;

use chainsync::{AccountProperty, ChainSyncAccount, Error};
use engine::testkit::TestEngine;
use hyper::StatusCode;
use magicblock_chainsync as chainsync;
use nucleus::testkit::V42_ID;
use solana_account::{AccountMode, ReadableAccount};
use solana_pubkey::Pubkey;
use support::{
    ACCOUNT_DATA, Account, CONTEXT_SLOT, FIRST_SUBSCRIPTION, RPC_BATCH_LIMIT, V42_INITIAL, acquire,
    delegation, http, scripted, start_chain_sync, websocket, within, yellowstone,
};

const PROMOTION_SLOT: u64 = 8;
const UPDATED_DATA: u8 = 2;
const CANARY_SUBSCRIPTION: u64 = FIRST_SUBSCRIPTION + 1;

/// Overlapping acquisitions share one fetch after acknowledgement; stale and duplicate WS updates never materialize.
#[tokio::test]
async fn overlapping_acquisitions_subscribe_before_fetch_and_skip_stale_updates() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let key = Pubkey::new_unique();
    let accounts = [ChainSyncAccount {
        pubkey: key,
        property: AccountProperty::Readonly,
    }];
    let script = async {
        let call = ws.next().await;
        assert_eq!(call.pubkey, key);
        assert!(
            !http.pending(),
            "HTTP cannot begin before subscription acknowledgement"
        );
        call.ack(FIRST_SUBSCRIPTION);
        let call = http.next().await;
        assert_eq!(call.keys().len(), 1);
        call.respond_accounts(CONTEXT_SLOT, &[Some(Account::new(key))]);
    };
    let (first, second, ()) = within(async {
        tokio::join!(
            acquire(&chain_sync, &accounts),
            acquire(&chain_sync, &accounts),
            script
        )
    })
    .await;
    first.unwrap();
    second.unwrap();
    assert!(
        !http.pending(),
        "overlapping acquisition must recheck presence under its lease"
    );
    assert_eq!(engine.get_account(key).unwrap().data(), ACCOUNT_DATA);
    let canary = Pubkey::new_unique();
    let script = async {
        ws.next().await.ack(CANARY_SUBSCRIPTION);
        http.next().await.respond_accounts(CONTEXT_SLOT, &[Some(Account::new(canary))]);
    };
    scripted(
        acquire(
            &chain_sync,
            &[ChainSyncAccount {
                pubkey: canary,
                property: AccountProperty::Readonly,
            }],
        ),
        script,
    )
    .await
    .unwrap();
    let mut updates = engine.accounts().subscribe(key);
    let mut image = Account::new(key);
    image.data = vec![UPDATED_DATA];
    ws.notify(FIRST_SUBSCRIPTION, CONTEXT_SLOT + 1, &image);
    within(async {
        loop {
            let update = updates.recv().await.unwrap();
            if update.slot() == CONTEXT_SLOT + 1 {
                assert_eq!(update.data(), &[UPDATED_DATA]);
                break;
            }
        }
    })
    .await;
    image.data = vec![9];
    let mut canaries = engine.accounts().subscribe(canary);
    // The canary follows each rejected update on the same socket and worker queue.
    // Its materialization makes the subsequent absence-of-update assertion meaningful.
    for (slot, barrier_slot) in
        [(CONTEXT_SLOT + 1, CONTEXT_SLOT + 2), (CONTEXT_SLOT, CONTEXT_SLOT + 3)]
    {
        ws.notify(FIRST_SUBSCRIPTION, slot, &image);
        ws.notify(CANARY_SUBSCRIPTION, barrier_slot, &Account::new(canary));
        within(async { while canaries.recv().await.unwrap().slot() != barrier_slot {} }).await;
        assert_eq!(engine.get_account(key).unwrap().data(), &[UPDATED_DATA]);
        assert_eq!(engine.get_account(key).unwrap().slot(), CONTEXT_SLOT + 1);
        assert!(
            updates.try_recv().is_err(),
            "skipped updates must not submit materialization"
        );
    }
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// Writable companions obey the RPC limit, and undelegated accounts refetch after subscribing.
#[tokio::test]
async fn delegation_companions_count_toward_rpc_batch_limit() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let accounts: Vec<_> = (0..=RPC_BATCH_LIMIT / 2)
        .map(|_| ChainSyncAccount {
            pubkey: Pubkey::new_unique(),
            property: AccountProperty::Writable,
        })
        .collect();
    let script = async {
        // Each writable primary has a record position, even when that record is absent.
        for expected in [RPC_BATCH_LIMIT, 2] {
            let call = http.next().await;
            assert_eq!(call.keys().len(), expected);
            let images: Vec<_> = call
                .keys()
                .enumerate()
                .map(|(index, key)| (index % 2 == 0).then(|| Account::new(key)))
                .collect();
            call.respond_accounts(CONTEXT_SLOT, &images);
        }
        for index in 0..accounts.len() {
            let call = ws.next().await;
            assert!(
                !http.pending(),
                "refetch must wait for subscription acknowledgements"
            );
            call.ack(FIRST_SUBSCRIPTION + index as u64);
        }
        let call = http.next().await;
        assert_eq!(call.keys().len(), accounts.len());
        assert_eq!(call.min_context_slot(), Some(CONTEXT_SLOT));
        let images: Vec<_> = call.keys().map(|key| Some(Account::new(key))).collect();
        call.respond_accounts(CONTEXT_SLOT + 1, &images);
    };
    let result = scripted(chain_sync.ensure(&accounts), script).await;
    assert_eq!(result.unwrap(), accounts.len() as u64 * 3);
    for request in &accounts {
        let account = engine.get_account(request.pubkey).unwrap();
        assert_eq!(account.data(), ACCOUNT_DATA);
        assert_eq!(account.slot(), CONTEXT_SLOT + 1);
    }
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// HTTP and partial subscription failures release references and leases; each acquisition can retry successfully.
#[tokio::test]
async fn failed_fetches_and_partial_subscriptions_allow_reacquisition() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let key = Pubkey::new_unique();
    let accounts = [ChainSyncAccount {
        pubkey: key,
        property: AccountProperty::Readonly,
    }];
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION);
        http.next().await.respond(StatusCode::OK, "malformed");
    };
    let result = scripted(acquire(&chain_sync, &accounts), script).await;
    assert!(matches!(result, Err(Error::Fetch(_))));
    assert!(engine.get_account(key).is_none());
    // A new provider ID proves failed acquisition released its logical reference;
    // successful materialization also proves it did not retain the Engine lease.
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION + 1);
        http.next().await.respond_accounts(CONTEXT_SLOT + 1, &[Some(Account::new(key))]);
    };
    scripted(acquire(&chain_sync, &accounts), script).await.unwrap();
    assert_eq!(engine.get_account(key).unwrap().data(), ACCOUNT_DATA);

    let accounts = [
        ChainSyncAccount {
            pubkey: Pubkey::new_unique(),
            property: AccountProperty::Readonly,
        },
        ChainSyncAccount {
            pubkey: Pubkey::new_unique(),
            property: AccountProperty::Readonly,
        },
    ];
    let script = async {
        ws.next().await.ack(3);
        ws.next().await.reject();
    };
    let result = scripted(acquire(&chain_sync, &accounts), script).await;
    assert!(result.is_err());
    assert!(!http.pending());
    let script = async {
        ws.next().await.ack(4);
        ws.next().await.ack(5);
        let call = http.next().await;
        let images: Vec<_> = call.keys().map(|key| Some(Account::new(key))).collect();
        call.respond_accounts(CONTEXT_SLOT + 1, &images);
    };
    scripted(acquire(&chain_sync, &accounts), script).await.unwrap();
    for request in &accounts {
        let account = engine
            .get_account(request.pubkey)
            .expect("successful retry must leave every requested account materialized");
        assert_eq!(account.data(), ACCOUNT_DATA);
        assert_eq!(account.slot(), CONTEXT_SLOT + 1);
    }
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}

/// HTTP promotion reuses tracking and freshness, retires delegated mirrors, and preserves null/record alignment.
#[tokio::test]
async fn http_promotion_preserves_freshness_tracking_and_null_alignment() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let key = Pubkey::new_unique();
    let (account, record) = delegation(key, V42_ID, engine.authority(), PROMOTION_SLOT, &[]);
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let accounts = [ChainSyncAccount {
        pubkey: key,
        property: AccountProperty::Readonly,
    }];
    let script = async {
        ws.next().await.ack(FIRST_SUBSCRIPTION);
        http.next().await.respond_accounts(PROMOTION_SLOT + 2, &[Some(account.clone())]);
        // DLP ownership promotes readonly acquisition to a companion fetch without
        // a second subscription or a weaker response freshness floor.
        let call = http.next().await;
        assert_eq!(call.min_context_slot(), Some(PROMOTION_SLOT + 2));
        assert_eq!(call.keys().collect::<Vec<_>>(), [key, record.key]);
        call.respond_accounts(PROMOTION_SLOT + 3, &[Some(account), Some(record)]);
    };
    scripted(acquire(&chain_sync, &accounts), script).await.unwrap();
    let account = engine.get_account(key).unwrap();
    assert_eq!(account.owner(), &V42_ID);
    assert_eq!(account.slot(), PROMOTION_SLOT);
    assert_eq!(account.mode(), AccountMode::Delegated);
    assert_eq!(ws.unsubscribed().await, FIRST_SUBSCRIPTION);

    let mut keys = [Pubkey::new_unique(), Pubkey::new_unique()];
    keys.sort_unstable();
    let accounts = [
        ChainSyncAccount {
            pubkey: keys[0],
            property: AccountProperty::Writable,
        },
        ChainSyncAccount {
            pubkey: keys[1],
            property: AccountProperty::Writable,
        },
    ];
    let (account, record) = delegation(keys[1], V42_ID, engine.authority(), CONTEXT_SLOT - 1, &[]);
    let script = async {
        // Missing primary and record positions must not shift the following pair.
        http.next()
            .await
            .respond_accounts(CONTEXT_SLOT, &[None, None, Some(account), Some(record)]);
        let call = ws.next().await;
        assert_eq!(call.pubkey, keys[0]);
        assert!(
            !http.pending(),
            "an absent writable account also needs live updates"
        );
        call.ack(FIRST_SUBSCRIPTION + 1);
        let call = http.next().await;
        assert_eq!(call.min_context_slot(), Some(CONTEXT_SLOT));
        assert_eq!(call.keys().collect::<Vec<_>>(), vec![keys[0]]);
        call.respond_accounts(CONTEXT_SLOT + 1, &[None]);
    };
    scripted(chain_sync.ensure(&accounts), script).await.unwrap();
    let restored = engine.get_account(keys[1]).unwrap();
    assert_eq!(restored.mode(), AccountMode::Delegated);
    assert_eq!(restored.owner(), &V42_ID);
    assert_eq!(restored.slot(), CONTEXT_SLOT - 1);
    assert_eq!(restored.data(), V42_INITIAL.to_le_bytes());
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}
