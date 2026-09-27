use std::time::Duration;

use engine::{
    pacemaker::ExternalBlock,
    testkit::{Pacing, TestEngine},
};
use keeper::testkit::{Dirs, block, keeper_builder};
use nucleus::runtime::BlockInput;
use setup::RpcTestEnv;
use solana_pubkey::Pubkey;

mod setup;

/// Verifies the mocked `getSlotLeaders` RPC method.
#[tokio::test]
async fn test_get_slot_leaders() {
    let env = RpcTestEnv::new().await;
    let leaders = env
        .rpc
        .get_slot_leaders(0, 1)
        .await
        .expect("get_slot_leaders request failed");

    assert_eq!(leaders.len(), 1, "should return a single leader");
    assert_eq!(
        leaders[0],
        env.engine.authority(),
        "leader should be the validator's own identity"
    );
}

/// Verifies the mocked `getFirstAvailableBlock` RPC method.
#[tokio::test]
async fn test_get_first_available_block() {
    let env = RpcTestEnv::new().await;
    let block = env
        .rpc
        .get_first_available_block()
        .await
        .expect("get_first_available_block request failed");

    assert_eq!(block, 0, "first available block should be 0");
}

/// Verifies the mocked `getLargestAccounts` RPC method.
#[tokio::test]
async fn test_get_largest_accounts() {
    let env = RpcTestEnv::new().await;
    let response = env
        .rpc
        .get_largest_accounts_with_config(Default::default())
        .await
        .expect("get_largest_accounts request failed");

    assert!(
        response.value.is_empty(),
        "largest accounts should return an empty list"
    );
}

/// Verifies the mocked `getTokenLargestAccounts` RPC method.
#[tokio::test]
async fn test_get_token_largest_accounts() {
    let env = RpcTestEnv::new().await;
    let accounts = env
        .rpc
        .get_token_largest_accounts(&Pubkey::new_unique())
        .await
        .expect("get_token_largest_accounts request failed");

    assert!(
        accounts.is_empty(),
        "token largest accounts should return an empty list"
    );
}

/// Verifies the mocked `getTokenSupply` RPC method.
#[tokio::test]
async fn test_get_token_supply() {
    let env = RpcTestEnv::new().await;
    let supply = env
        .rpc
        .get_token_supply(&Pubkey::new_unique())
        .await
        .expect("get_token_supply request failed");

    // The mocked response for a non-existent mint returns default values.
    assert_eq!(supply.amount, "0", "token supply amount should be '0'");
    assert_eq!(supply.decimals, 0, "token supply decimals should be 0");
}

/// Verifies the mocked `getSupply` RPC method.
#[tokio::test]
async fn test_get_supply() {
    let env = RpcTestEnv::new().await;
    let supply_info =
        env.rpc.supply().await.expect("get_supply request failed");

    assert_eq!(
        supply_info.value.total,
        u64::MAX,
        "total supply should be u64::MAX"
    );
    assert_eq!(
        supply_info.value.circulating,
        u64::MAX / 2,
        "circulating supply should be u64::MAX / 2"
    );
    assert!(
        supply_info.value.non_circulating_accounts.is_empty(),
        "non-circulating accounts should be empty"
    );
}

/// Verifies the mocked `getHighestSnapshotSlot` RPC method.
#[tokio::test]
async fn test_get_highest_snapshot_slot() {
    let env = RpcTestEnv::new().await;
    let snapshot_info = env
        .rpc
        .get_highest_snapshot_slot()
        .await
        .expect("get_highest_snapshot_slot request failed");

    assert_eq!(snapshot_info.full, 0, "full snapshot slot should be 0");
    assert!(
        snapshot_info.incremental.is_none(),
        "incremental snapshot should be None"
    );
}

/// Verifies the `getHealth` RPC method.
#[tokio::test]
async fn test_get_health() {
    let env = RpcTestEnv::new().await;
    let health = env.rpc.get_health().await;

    assert!(health.is_ok());
}

/// Verifies the mocked `getGenesisHash` RPC method.
#[tokio::test]
async fn test_get_genesis_hash() {
    let env = RpcTestEnv::new().await;
    let genesis_hash = env
        .rpc
        .get_genesis_hash()
        .await
        .expect("get_genesis_hash request failed");

    assert_eq!(
        genesis_hash,
        Default::default(),
        "genesis hash should be the default hash"
    );
}

/// Verifies slot-derived epoch progress across boundaries and skipped slots,
/// including the fallback schedule and seals that do not advance the slot.
#[tokio::test]
async fn test_get_epoch_info() {
    for (superblock, slots) in [(7, 7), (0, 432_000)] {
        let dirs = Dirs::default();
        let mut builder = keeper_builder(&dirs);
        builder.blockstore.superblock = superblock;
        let engine =
            TestEngine::from_builder(dirs, builder, Pacing::External).await;
        let mut env = RpcTestEnv::with_engine(engine).await;
        let schedule = env.rpc.get_epoch_schedule().await.unwrap();
        assert_eq!(schedule, *env.engine.epoch_schedule());
        assert_eq!(schedule.slots_per_epoch, slots);
        assert_eq!(schedule.leader_schedule_slot_offset, slots);

        for slot in [0, slots - 1, slots, slots + 1, 3 * slots + 2] {
            let clock = env.engine.clock(env.engine.blocks().latest());
            if slot != 0 {
                let (boundary, submitted) =
                    ExternalBlock::new(BlockInput::Production(block(slot)));
                env.engine.pacer().send(boundary).await.unwrap();
                tokio::time::timeout(Duration::from_secs(4), submitted)
                    .await
                    .unwrap()
                    .unwrap();
            }
            let info = env.rpc.get_epoch_info().await.unwrap();
            assert_eq!(info.absolute_slot, slot);
            assert_eq!(info.epoch, slot / slots);
            assert_eq!(info.slot_index, slot % slots);
            assert_eq!(info.slots_in_epoch, slots);
            // Clock describes the executing slot, which RPC reports only once completed.
            if clock.slot == info.absolute_slot {
                assert_eq!(info.epoch, clock.epoch);
            }
        }

        if superblock == 0 {
            let before = env.rpc.get_epoch_info().await.unwrap();
            // External pacing is idle; the barrier excludes execution during the snapshot.
            let guard = env.engine.barrier().await.unwrap();
            let sealed = env.engine.finalize_superblock(None).unwrap();
            tokio::time::timeout(Duration::from_secs(4), sealed)
                .await
                .unwrap()
                .unwrap();
            drop(guard);
            let after = env.rpc.get_epoch_info().await.unwrap();
            assert_eq!(
                after, before,
                "sealing must not advance epoch progress"
            );
        }
        env.engine.shutdown().terminate().await;
    }
}

/// Verifies every schedule field matches the schedule exposed by Engine.
#[tokio::test]
async fn test_get_epoch_schedule() {
    let env = RpcTestEnv::new().await;
    let schedule = env
        .rpc
        .get_epoch_schedule()
        .await
        .expect("get_epoch_schedule request failed");

    assert_eq!(schedule, *env.engine.epoch_schedule());
    assert!(!schedule.warmup, "warmup should be false");
}

/// Verifies the mocked `getClusterNodes` RPC method.
#[tokio::test]
async fn test_get_cluster_nodes() {
    let env = RpcTestEnv::new().await;
    let nodes = env
        .rpc
        .get_cluster_nodes()
        .await
        .expect("get_cluster_nodes request failed");

    assert_eq!(nodes.len(), 1, "should be exactly one node in the cluster");
    assert_eq!(
        nodes[0].pubkey,
        env.engine.authority().to_string(),
        "node pubkey should match validator identity"
    );
}
