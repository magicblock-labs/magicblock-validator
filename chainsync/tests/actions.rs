#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Dependency-first execution through the real v42 program.

mod transport;

use chainsync::{AccountProperty, ChainSyncAccount};
use engine::testkit::TestEngine;
use keeper::testkit::store_v42;
use magicblock_chainsync as chainsync;
use nucleus::testkit::V42_ID;
use nucleus::testkit::v42_sum;
use solana_account::{AccountMode, ReadableAccount};
use solana_pubkey::Pubkey;
use transport::{
    ACTION_AMOUNT, DELEGATION_SLOT, FIRST_SUBSCRIPTION, V42_INITIAL, acquire, delegation, http,
    scripted, start_chain_sync, websocket, within, yellowstone,
};
use v42_calculator_interface::builder::transfer;

/// Nested delegated dependencies run their v42 actions before the parent consumes their resulting state.
#[tokio::test]
async fn nested_postdelegation_actions_execute_dependency_first() {
    let mut engine = TestEngine::new().await;
    let mut http = http::Server::new().await;
    let mut ws = websocket::Server::new().await;
    let grpc = yellowstone::Server::new().await;
    let root = Pubkey::new_unique();
    let child = Pubkey::new_unique();
    let recipient = store_v42(&engine, 0, AccountMode::Delegated);
    let mut logs = engine.transactions().subscribe_logs(root).await;
    let (root_account, root_record) = delegation(
        root,
        V42_ID,
        engine.authority(),
        DELEGATION_SLOT - 2,
        &[v42_sum(root, &[child])],
    );
    let (child_account, child_record) = delegation(
        child,
        V42_ID,
        engine.authority(),
        DELEGATION_SLOT - 2,
        &[transfer(child, recipient, ACTION_AMOUNT)],
    );
    let chain_sync = start_chain_sync(&mut engine, &http, Some(&ws), &grpc);
    ws.wait_for_connection().await;
    let accounts = [ChainSyncAccount {
        pubkey: root,
        property: AccountProperty::Writable,
    }];
    let script = async {
        http.next()
            .await
            .respond_accounts(DELEGATION_SLOT, &[Some(root_account), Some(root_record)]);
        // The parent's readonly action dependency discovers a second delegation.
        // Resolving its writable promotion reacquires the child after releasing its lease.
        let call = ws.next().await;
        assert_eq!(call.pubkey, child);
        call.ack(FIRST_SUBSCRIPTION);
        http.next()
            .await
            .respond_accounts(DELEGATION_SLOT, &[Some(child_account.clone())]);
        http.next()
            .await
            .respond_accounts(DELEGATION_SLOT, &[Some(child_account), Some(child_record)]);
    };
    scripted(acquire(&chain_sync, &accounts), script).await.unwrap();
    assert_eq!(
        engine.get_account(child).unwrap().data(),
        (V42_INITIAL - ACTION_AMOUNT).to_le_bytes()
    );
    assert_eq!(
        engine.get_account(recipient).unwrap().data(),
        ACTION_AMOUNT.to_le_bytes()
    );
    // PostFinalize invokes v42 through CPI, so calculator results are return-data, not writes.
    let logs = within(logs.recv()).await.unwrap();
    let result = format!("v42: result={} -> return_data", V42_INITIAL - ACTION_AMOUNT);
    assert!(logs.logs.iter().any(|line| line.contains(&result)));
    drop(chain_sync);
    engine.close().await;
    ws.close().await;
    http.close().await;
    grpc.close().await;
}
