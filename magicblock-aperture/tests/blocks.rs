use magicblock_core::link::blocks::BlockHash;
use setup::RpcTestEnv;
use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status::UiTransactionEncoding;

mod setup;

/// Verifies `get_slot` returns a valid slot number that progresses over time.
#[tokio::test]
async fn test_get_slot() {
    let env = RpcTestEnv::new().await;
    let initial_slot =
        env.rpc.get_slot().await.expect("get_slot request failed");

    // Wait for at least 2 slots to progress, demonstrating auto-advancement
    env.wait_for_slot_progress(2).await;

    let new_slot = env.rpc.get_slot().await.expect("get_slot request failed");
    assert!(
        new_slot >= initial_slot + 2,
        "slot should have progressed by at least 2: initial={initial_slot}, new={new_slot}"
    );
}

/// Verifies `get_block_height` returns a valid block height.
#[tokio::test]
async fn test_get_block_height() {
    let env = RpcTestEnv::new().await;
    let block_height = env
        .rpc
        .get_block_height()
        .await
        .expect("get_block_height request failed");
    // Block height should be at least 1 (genesis slot moves to slot 1 during setup)
    assert!(block_height >= 1, "block height should be at least 1");
}

/// Verifies `get_latest_blockhash` returns a valid blockhash.
#[tokio::test]
async fn test_get_latest_blockhash() {
    let env = RpcTestEnv::new().await;
    // Wait for at least one slot to ensure a non-genesis blockhash exists
    env.wait_for_slot_progress(1).await;

    // Test the method with commitment level (call this first to get both values together)
    let (blockhash, last_valid_slot) = env
        .rpc
        .get_latest_blockhash_with_commitment(Default::default())
        .await
        .expect("failed to request blockhash with commitment");

    // Test the basic method - may return a different blockhash if slot advanced
    let rpc_blockhash = env
        .rpc
        .get_latest_blockhash()
        .await
        .expect("get_latest_blockhash request failed");

    // Both blockhashes should be valid (non-default)
    assert_ne!(
        blockhash,
        BlockHash::default(),
        "blockhash should not be default"
    );
    assert_ne!(
        rpc_blockhash,
        BlockHash::default(),
        "rpc_blockhash should not be default"
    );

    // last_valid_slot should be greater than current slot
    let current_slot =
        env.rpc.get_slot().await.expect("get_slot request failed");
    assert!(
        last_valid_slot > current_slot,
        "last_valid_block_height ({last_valid_slot}) should be greater than current slot ({current_slot})"
    );
}

/// Verifies `is_blockhash_valid` for both valid and invalid cases.
#[tokio::test]
async fn test_is_blockhash_valid() {
    let env = RpcTestEnv::new().await;
    // Wait for at least one slot to ensure a valid blockhash exists
    env.wait_for_slot_progress(1).await;

    // Get a recent blockhash
    let latest_blockhash = env
        .rpc
        .get_latest_blockhash()
        .await
        .expect("get_latest_blockhash request failed");

    // Test a recent, valid blockhash
    let is_valid = env
        .rpc
        .is_blockhash_valid(&latest_blockhash, Default::default())
        .await
        .expect("request for recent blockhash failed");
    assert!(is_valid, "a recent blockhash should be considered valid");

    // Test an unknown (and therefore invalid) blockhash
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

/// Verifies `get_block` can fetch a block with transactions.
#[tokio::test]
async fn test_get_block() {
    let env = RpcTestEnv::new().await;
    // Record the slot before executing the transaction
    let slot_before =
        env.rpc.get_slot().await.expect("get_slot request failed");

    let signature = env.execute_transaction().await;

    // Wait for at least one slot to progress
    env.wait_for_slot_progress(1).await;

    // The transaction should be in a slot >= slot_before
    let current_slot =
        env.rpc.get_slot().await.expect("get_slot request failed");

    // Find the block containing our transaction by checking recent slots
    let mut found_block = None;
    for slot in slot_before..=current_slot {
        if let Ok(block) = env
            .rpc
            .get_block_with_config(
                slot,
                RpcBlockConfig {
                    encoding: Some(UiTransactionEncoding::Base64),
                    ..Default::default()
                },
            )
            .await
        {
            if let Some(transactions) = &block.transactions {
                for txn in transactions {
                    if let Some(decoded) = txn.transaction.decode() {
                        if decoded.signatures[0] == signature {
                            found_block = Some((slot, block));
                            break;
                        }
                    }
                }
            }
        }
        if found_block.is_some() {
            break;
        }
    }

    let (slot, block) =
        found_block.expect("should find block containing transaction");

    assert_eq!(block.block_height, Some(slot));

    // Test fetching a non-existent block, which should result in an error
    let nonexistent_block_result = env.rpc.get_block(current_slot + 100).await;
    assert!(
        nonexistent_block_result.is_err(),
        "request for a non-existent block should fail"
    );
}

/// Verifies `get_blocks` can fetch a specific range of slots.
#[tokio::test]
async fn test_get_blocks() {
    let env = RpcTestEnv::new().await;
    // Wait for at least 5 slots to exist
    env.wait_for_slot_progress(5).await;

    let current_slot =
        env.rpc.get_slot().await.expect("get_slot request failed");
    let start = current_slot.saturating_sub(3);
    let end = current_slot;

    let blocks = env
        .rpc
        .get_blocks(start, Some(end))
        .await
        .expect("get_blocks request failed");
    // Should return a contiguous range from start to end
    assert!(!blocks.is_empty(), "should return at least one block");

    // Verify each slot is within bounds [start, end]
    for &slot in &blocks {
        assert!(
            slot >= start,
            "slot {slot} should be >= start {start}"
        );
        assert!(
            slot <= end,
            "slot {slot} should be <= end {end}"
        );
    }

    // Verify slots are strictly increasing and contiguous
    for i in 1..blocks.len() {
        let prev = blocks[i - 1];
        let curr = blocks[i];
        assert!(
            curr > prev,
            "slots should be strictly increasing: {prev} -> {curr}"
        );
        assert_eq!(
            curr,
            prev + 1,
            "slots should be contiguous: expected {} after {}, got {}",
            prev + 1,
            prev,
            curr
        );
    }
}

/// Verifies `get_block_time` returns a valid Unix timestamp for a slot.
#[tokio::test]
async fn test_get_block_time() {
    let env = RpcTestEnv::new().await;
    // Wait for at least one slot
    env.wait_for_slot_progress(1).await;

    let current_slot =
        env.rpc.get_slot().await.expect("get_slot request failed");

    let time = env
        .rpc
        .get_block_time(current_slot)
        .await
        .expect("get_block_time request failed");

    // The timestamp should be a reasonable Unix timestamp (> 1 billion)
    assert!(
        time > 1_000_000_000,
        "get_block_time should return a valid Unix timestamp, got {time}"
    );
}

/// Verifies `get_blocks_with_limit` can fetch a limited number of slots.
#[tokio::test]
async fn test_get_blocks_with_limit() {
    let env = RpcTestEnv::new().await;
    // Wait for at least 10 slots to exist
    env.wait_for_slot_progress(10).await;

    let current_slot =
        env.rpc.get_slot().await.expect("get_slot request failed");
    let start_slot = current_slot.saturating_sub(7);
    let limit = 3;

    let blocks = env
        .rpc
        .get_blocks_with_limit(start_slot, limit)
        .await
        .expect("get_blocks_with_limit request failed");

    // Should return exactly `limit` blocks
    assert_eq!(blocks.len(), limit, "should return exactly {limit} blocks");
    // First block should be start_slot
    assert_eq!(
        blocks.first().copied().unwrap_or(0),
        start_slot,
        "first block should be start_slot"
    );

    // Verify slots are strictly increasing and contiguous
    for i in 1..blocks.len() {
        let prev = blocks[i - 1];
        let curr = blocks[i];
        assert!(
            curr > prev,
            "slots should be strictly increasing: {prev} -> {curr}"
        );
        assert_eq!(
            curr,
            prev + 1,
            "slots should be contiguous: expected {} after {}, got {}",
            prev + 1,
            prev,
            curr
        );
    }
}
