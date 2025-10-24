use magicblock_core::link::blocks::BlockHash;
use setup::RpcTestEnv;
use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status::UiTransactionEncoding;

mod setup;

/// Verifies `get_slot` consistently returns the latest slot number.
#[tokio::test]
async fn test_get_slot() {
    let env = RpcTestEnv::new().await;
    // Check repeatedly while advancing slots to ensure it stays in sync.
    for _ in 0..8 {
        let slot = env.rpc.get_slot().await.expect("get_slot request failed");
        assert_eq!(
            slot,
            env.latest_slot(),
            "RPC slot should match the latest slot in the ledger"
        );
        env.advance_slots(1);
    }
}

/// Verifies `get_block_height` returns the latest slot number.
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
        "RPC block height should match the latest slot"
    );
}

/// Verifies `get_latest_blockhash` and its commitment-aware variant.
#[tokio::test]
async fn test_get_latest_blockhash() {
    let env = RpcTestEnv::new().await;
    env.advance_slots(1); // Ensure a non-genesis blockhash exists.

    // Test the basic method.
    let rpc_blockhash = env
        .rpc
        .get_latest_blockhash()
        .await
        .expect("get_latest_blockhash request failed");

    let latest_block = env.block.load();
    assert_eq!(
        rpc_blockhash, latest_block.blockhash,
        "RPC blockhash should match the latest from the ledger"
    );

    // Test the method with commitment level, which also returns the last valid slot.
    let (blockhash, last_valid_slot) = env
        .rpc
        .get_latest_blockhash_with_commitment(Default::default())
        .await
        .expect("failed to request blockhash with commitment");

    assert_eq!(
        blockhash, latest_block.blockhash,
        "RPC blockhash with commitment should also match"
    );
    assert!(
        last_valid_slot >= latest_block.slot + 150,
        "last_valid_block_height is incorrect"
    );
}

/// Verifies `is_blockhash_valid` for both valid and invalid cases.
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
        .expect("request for recent blockhash failed");
    assert!(is_valid, "a recent blockhash should be considered valid");

    // Test an unknown (and therefore invalid) blockhash.
    let invalid_blockhash = BlockHash::new_unique();
    let is_valid = env
        .rpc
        .is_blockhash_valid(&invalid_blockhash, Default::default())
        .await
        .expect("request for invalid blockhash failed");
    assert!(
        !is_valid,
        "an unknown blockhash should be considered invalid"
    );
}

/// Verifies `get_block` can fetch a full block and its contents.
#[tokio::test]
async fn test_get_block() {
    let env = RpcTestEnv::new().await;
    let signature = env.execute_transaction().await;
    let latest_slot = env.block.load().slot;
    let latest_blockhash = env.block.load().blockhash;

    // Test fetching an existing block with a specific config.
    let block = env
        .rpc
        .get_block_with_config(
            latest_slot,
            RpcBlockConfig {
                encoding: Some(UiTransactionEncoding::Base64),
                ..Default::default()
            },
        )
        .await
        .expect("get_block request for an existing block failed");

    assert_eq!(block.block_height, Some(latest_slot));
    assert_eq!(block.blockhash, latest_blockhash.to_string());

    let first_transaction = block
        .transactions
        .expect("block should contain transactions")
        .pop()
        .expect("transaction list should not be empty");

    let block_txn_signature =
        first_transaction.transaction.decode().unwrap().signatures[0];
    assert_eq!(block_txn_signature, signature);

    // Test fetching a non-existent block, which should result in an error.
    let nonexistent_block_result = env.rpc.get_block(latest_slot + 100).await;
    assert!(
        nonexistent_block_result.is_err(),
        "request for a non-existent block should fail"
    );
}

/// Verifies `get_blocks` can fetch a specific range of slots.
#[tokio::test]
async fn test_get_blocks() {
    let env = RpcTestEnv::new().await;
    env.advance_slots(5);

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

/// Verifies `get_block_time` returns the correct Unix timestamp for a slot.
#[tokio::test]
async fn test_get_block_time() {
    let env = RpcTestEnv::new().await;
    let latest_block = env.block.load();

    let time = env
        .rpc
        .get_block_time(latest_block.slot)
        .await
        .expect("get_block_time request failed");
    assert_eq!(
        time, latest_block.clock.unix_timestamp,
        "get_block_time should return the same timestamp stored in the ledger"
    );
}

/// Verifies `get_blocks_with_limit` can fetch a limited number of slots from a start point.
#[tokio::test]
async fn test_get_blocks_with_limit() {
    let env = RpcTestEnv::new().await;
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
