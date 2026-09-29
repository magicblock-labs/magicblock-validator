use setup::RpcTestEnv;
use solana_hash::Hash;
use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status::UiTransactionEncoding;

mod setup;

#[tokio::test]
async fn block_rpc_methods() {
    let mut env = RpcTestEnv::new().await;
    let initial = env.rpc.get_slot().await.expect("get slot");

    env.engine.advance(2).await;
    drop(
        env.engine
            .barrier()
            .await
            .expect("advanced blocks are applied"),
    );
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
    assert!(
        env.rpc
            .is_blockhash_valid(&blockhash, Default::default())
            .await
            .unwrap()
    );
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
    assert!(
        block
            .transactions
            .is_some_and(|transactions| !transactions.is_empty())
    );
    assert_eq!(
        env.rpc.get_block_time(transaction_slot).await.unwrap(),
        transaction_slot as i64
    );
    assert!(env.rpc.get_block(transaction_slot + 100).await.is_err());

    env.engine.advance(10).await;
    drop(
        env.engine
            .barrier()
            .await
            .expect("advanced blocks are applied"),
    );
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
}
