use setup::RpcTestEnv;
use test_kit::Signer;

mod setup;

/// Verifies the `getVersion` RPC method returns valid information.
#[tokio::test]
async fn test_get_version() {
    let env = RpcTestEnv::new().await;
    let version_info = env
        .rpc
        .get_version()
        .await
        .expect("get_version request failed");

    assert!(
        !version_info.solana_core.is_empty(),
        "solana version should not be empty"
    );
    assert!(
        version_info.feature_set.is_some(),
        "feature set info should be present"
    );
}

/// Verifies the `getIdentity` RPC method returns the correct validator public key.
#[tokio::test]
async fn test_get_identity() {
    let env = RpcTestEnv::new().await;
    let identity = env
        .rpc
        .get_identity()
        .await
        .expect("get_identity request failed");

    assert_eq!(
        identity,
        env.execution.payer.pubkey(),
        "identity should match the validator's public key"
    );
}
