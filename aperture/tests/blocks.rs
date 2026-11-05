#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use engine::testkit::{Pacing, TestEngine};
use keeper::testkit::{Dirs, keeper_builder};
use setup::RpcTestEnv;
use solana_hash::Hash;
use solana_rpc_client_api::{
    client_error::ErrorKind,
    config::RpcBlockConfig,
    request::{RpcError, RpcRequest},
};
use solana_transaction_status::UiTransactionEncoding;

mod setup;

/// Proves retained block reads, slot listings, and availability boundaries agree.
#[tokio::test]
async fn block_rpc_methods() {
    let mut env = RpcTestEnv::new().await;
    let initial = env.rpc.get_slot().await.expect("get slot");

    env.engine.advance(2).await;
    drop(env.engine.barrier().await.expect("advanced blocks are applied"));
    let current = env.rpc.get_slot().await.expect("get advanced slot");
    assert!(current > initial);
    assert_eq!(
        env.rpc.get_block_height().await.expect("get block height"),
        current
    );

    let (blockhash, last_valid_slot) = env
        .rpc
        .get_latest_blockhash_with_commitment(Default::default())
        .await
        .expect("get latest blockhash");
    assert_ne!(blockhash, Hash::default());
    assert_eq!(env.rpc.get_latest_blockhash().await.unwrap(), blockhash);
    assert!(last_valid_slot > current);
    assert!(env.rpc.is_blockhash_valid(&blockhash, Default::default()).await.unwrap());
    assert!(
        !env.rpc
            .is_blockhash_valid(&Hash::new_unique(), Default::default())
            .await
            .unwrap()
    );

    let signature = env.execute_write().await;
    env.engine.advance(1).await;
    env.engine.sync().await;
    let transaction_slot = env
        .engine
        .transactions()
        .get(signature)
        .await
        .expect("read transaction")
        .expect("stored transaction")
        .execution
        .header
        .slot;
    let block = env
        .rpc
        .get_block_with_config(
            transaction_slot,
            RpcBlockConfig {
                encoding: Some(UiTransactionEncoding::Base64),
                ..Default::default()
            },
        )
        .await
        .expect("get transaction block");
    assert_eq!(block.block_height, Some(transaction_slot));
    assert!(block.transactions.is_some_and(|transactions| !transactions.is_empty()));
    assert_eq!(
        env.rpc.get_block_time(transaction_slot).await.unwrap(),
        transaction_slot as i64
    );
    assert!(env.rpc.get_block(transaction_slot + 100).await.is_err());

    env.engine.advance(10).await;
    env.engine.sync().await;
    let latest = env.rpc.get_slot().await.unwrap();
    let start = latest - 3;
    assert_eq!(
        env.rpc.get_blocks(start, Some(latest)).await.unwrap(),
        (start..=latest).collect::<Vec<_>>()
    );

    let start = latest - 7;
    assert_eq!(
        env.rpc.get_blocks_with_limit(start, 3).await.unwrap(),
        (start..start + 3).collect::<Vec<_>>()
    );

    let first = *env.engine.blocks().retained_range().unwrap().start();
    assert_eq!(env.rpc.get_first_available_block().await.unwrap(), first);
    assert_eq!(env.rpc.minimum_ledger_slot().await.unwrap(), first);

    let parent = env.rpc.get_block(latest - 1).await.unwrap();
    let child = env.rpc.get_block(latest).await.unwrap();
    assert_eq!(child.previous_blockhash, parent.blockhash);
}

/// Proves pruning advances both boundaries, removes listed slots, and reports cleaned blocks.
#[tokio::test]
async fn pruned_block_history() {
    let dirs = Dirs::default();
    let mut builder = keeper_builder(&dirs);
    builder.ledger.size_limit = 0;
    builder.blockstore.superblock = 256;
    let engine = TestEngine::from_builder(dirs, builder, Pacing::External).await;
    let mut env = RpcTestEnv::with_engine(engine).await;
    env.engine.advance(514).await;
    env.engine.sync().await;
    let first = env.rpc.get_first_available_block().await.unwrap();
    assert!(first > 0);
    assert_eq!(env.rpc.minimum_ledger_slot().await.unwrap(), first);
    assert_eq!(
        env.rpc.get_blocks(0, Some(first)).await.unwrap(),
        vec![first]
    );
    assert_eq!(
        env.rpc.get_blocks_with_limit(0, 2).await.unwrap(),
        vec![first, first + 1]
    );
    for method in ["getBlock", "getBlockTime"] {
        let error = env
            .rpc
            .send::<json::Value>(RpcRequest::Custom { method }, json::json!([first - 1]))
            .await
            .unwrap_err();
        match error.kind() {
            ErrorKind::RpcError(RpcError::RpcResponseError { code, message, .. }) => {
                assert_eq!(*code, -32001);
                assert!(message.contains(&format!("First available block: {first}")));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
