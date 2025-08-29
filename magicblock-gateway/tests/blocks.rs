use magicblock_core::link::blocks::BlockHash;
use setup::RpcTestEnv;
use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status::UiTransactionEncoding;

mod setup;

#[tokio::test]
async fn test_get_slot() {
    let env = RpcTestEnv::new().await;
    for _ in 0..64 {
        let slot = env.rpc.get_slot().await.expect("get_slot request failed");
        assert_eq!(
            slot,
            env.latest_slot(),
            "RPC slot should match the latest slot in the ledger"
        );
        env.advance_slots(1);
    }
}

#[tokio::test]
async fn test_get_block_height() {
    let env = RpcTestEnv::new().await;
    let block_height = env
        .rpc
        .get_block_height()
        .await
        .expect("get_block_height request failed");
    assert_eq!(
        block_height,
        env.latest_slot(),
        "RPC block height should match the current slot of the AccountsDb"
    );
}

#[tokio::test]
async fn test_get_latest_blockhash() {
    let env = RpcTestEnv::new().await;
    // Advance a slot to ensure a non-genesis blockhash exists.
    env.advance_slots(1);

    let rpc_blockhash = env
        .rpc
        .get_latest_blockhash()
        .await
        .expect("get_latest_blockhash request failed");

    let latest_block = env.block.load();
    assert_eq!(
        rpc_blockhash, latest_block.blockhash,
        "RPC blockhash should match the latest blockhash from the ledger"
    );
    let (blockhash, slot) = env
        .rpc
        .get_latest_blockhash_with_commitment(Default::default())
        .await
        .expect("failed to request blockhash with commitment");
    assert_eq!(
        blockhash, latest_block.blockhash,
        "RPC blockhash should match the latest blockhash from the ledger"
    );
    assert!(
        slot > latest_block.slot + 150,
        "last_valid_block_height is incorrect"
    );
}

#[tokio::test]
async fn test_is_blockhash_valid() {
    let env = RpcTestEnv::new().await;
    env.advance_slots(1);

    // Test a recent, valid blockhash.
    let latest_block = env.block.load();
    let is_valid = env
        .rpc
        .is_blockhash_valid(&latest_block.blockhash, Default::default())
        .await
        .expect("is_blockhash_valid request for recent blockhash failed");
    assert!(is_valid, "a recent blockhash should be considered valid");

    // Test an invalid blockhash.
    let invalid_blockhash = BlockHash::new_unique();

    let is_invalid = !env
        .rpc
        .is_blockhash_valid(&invalid_blockhash, Default::default())
        .await
        .expect("is_blockhash_valid request for invalid blockhash failed");
    assert!(
        is_invalid,
        "an unknown blockhash should be considered invalid"
    );
}

#[tokio::test]
async fn test_get_block() {
    let env = RpcTestEnv::new().await;
    // Create a transaction in ledger and advance the slot to include it in a block.
    let signature = env.execute_transaction().await;
    let latest_block = env.block.load();
    env.advance_slots(1);

    // Test fetching an existing block.
    let block = env
        .rpc
        .get_block_with_config(
            latest_block.slot,
            RpcBlockConfig {
                encoding: Some(UiTransactionEncoding::Base64),
                ..Default::default()
            },
        )
        .await
        .expect("get_block request for an existing block failed");
    assert_eq!(
        block.block_height,
        Some(latest_block.slot),
        "block height mismatch"
    );
    assert_eq!(
        block.blockhash,
        latest_block.blockhash.to_string(),
        "blockhash of fetched block should match the latest in the ledger"
    );
    let transaction = block
        .transactions
        .expect("returned block should have transactions list included")
        .pop();
    assert!(
        transaction.is_some(),
        "block should contain the executed transaction"
    );
    let transaction = transaction.unwrap();
    let block_txn_signature =
        transaction.transaction.decode().unwrap().signatures[0];
    assert_eq!(
        block_txn_signature, signature,
        "block should contain the processed transaction"
    );

    // Test fetching a non-existent block.
    let nonexistent_block = env.rpc.get_block(latest_block.slot + 100).await;
    assert!(
        nonexistent_block.is_err(),
        "block should not exist at a future slot"
    );
}

#[tokio::test]
async fn test_get_blocks() {
    let env = RpcTestEnv::new().await;
    // Create 5 new blocks.
    env.advance_slots(5);

    // Request blocks from slot 1 to 4.
    let blocks = env
        .rpc
        .get_blocks(1, Some(4))
        .await
        .expect("get_blocks request failed");
    assert_eq!(
        blocks,
        vec![1, 2, 3, 4],
        "should return the correct range of slots"
    );
}

#[tokio::test]
async fn test_get_block_time() {
    let env = RpcTestEnv::new().await;
    let latest_block = env.block.load();

    // Request blocks from slot 1 to 4.
    let time = env
        .rpc
        .get_block_time(latest_block.slot)
        .await
        .expect("get_blocks request failed");
    assert_eq!(
        time, latest_block.clock.unix_timestamp,
        "get_block_time should return the same timestamp stored in the ledger"
    );
}

#[tokio::test]
async fn test_get_blocks_with_limit() {
    let env = RpcTestEnv::new().await;
    // Create 10 new blocks.
    env.advance_slots(10);
    let start_slot = 5;
    let limit = 3;

    let blocks = env
        .rpc
        .get_blocks_with_limit(start_slot, limit)
        .await
        .expect("get_blocks_with_limit request failed");
    assert_eq!(
        blocks,
        vec![5, 6, 7],
        "should return the correct range of slots with a limit"
    );
}
