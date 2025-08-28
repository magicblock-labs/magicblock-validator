use setup::RpcTestEnv;
use solana_pubkey::Pubkey;

mod setup;

#[tokio::test]
async fn test_get_slot_leaders() {
    let env = RpcTestEnv::new().await;
    let leaders = env
        .rpc
        .get_slot_leaders(0, 1)
        .await
        .expect("get_slot_leaders request failed");
    assert_eq!(leaders.len(), 1, "should return a single leader");
}

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
        "largest accounts should be an empty list"
    );
}

#[tokio::test]
async fn test_get_token_largest_accounts() {
    let env = RpcTestEnv::new().await;
    let response = env
        .rpc
        .get_token_largest_accounts(&Pubkey::new_unique()) // Mint pubkey is required
        .await
        .expect("get_token_largest_accounts request failed");
    assert!(
        response.is_empty(),
        "token largest accounts should be an empty list"
    );
}

#[tokio::test]
async fn test_get_token_supply() {
    let env = RpcTestEnv::new().await;
    let response = env
        .rpc
        .get_token_supply(&Pubkey::new_unique())
        .await
        .expect("get_token_supply request failed");
    assert_eq!(response.amount, "", "token supply amount should be absent");
    assert_eq!(response.decimals, 0, "token supply decimals should be 0");
}

#[tokio::test]
async fn test_get_supply() {
    let env = RpcTestEnv::new().await;
    let response = env.rpc.supply().await.expect("get_supply request failed");
    assert_eq!(response.value.total, 0, "total supply should be 0");
    assert_eq!(
        response.value.circulating, 0,
        "circulating supply should be 0"
    );
    assert!(
        response.value.non_circulating_accounts.is_empty(),
        "non-circulating accounts should be empty"
    );
}

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

#[tokio::test]
async fn test_get_health() {
    let env = RpcTestEnv::new().await;
    env.rpc
        .get_health()
        .await
        .expect("get_health request failed");
}

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

#[tokio::test]
async fn test_get_epoch_info() {
    let env = RpcTestEnv::new().await;
    let epoch_info = env
        .rpc
        .get_epoch_info()
        .await
        .expect("get_epoch_info request failed");
    assert_eq!(epoch_info.epoch, 0, "epoch should be 0");
    assert_eq!(epoch_info.absolute_slot, 0, "absolute_slot should be 0");
}

#[tokio::test]
async fn test_get_epoch_schedule() {
    let env = RpcTestEnv::new().await;
    let schedule = env
        .rpc
        .get_epoch_schedule()
        .await
        .expect("get_epoch_schedule request failed");
    assert_eq!(schedule.slots_per_epoch, 0, "slots_per_epoch should be 0");
    assert!(schedule.warmup, "warmup should be true");
}

#[tokio::test]
async fn test_get_cluster_nodes() {
    let env = RpcTestEnv::new().await;
    let nodes = env
        .rpc
        .get_cluster_nodes()
        .await
        .expect("get_cluster_nodes request failed");
    assert_eq!(nodes.len(), 1, "should be exactly one node in the cluster");
}
