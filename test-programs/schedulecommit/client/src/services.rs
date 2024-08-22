use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;

// NOTE: sync version of test-tools/src/services.rs
pub fn is_devnet_up() -> bool {
    RpcClient::new_with_commitment(
        "https://api.devnet.solana.com".to_string(),
        CommitmentConfig::processed(),
    )
    .get_version()
    .is_ok()
}

#[macro_export]
macro_rules! skip_if_devnet_down {
    () => {
        if !$crate::services::is_devnet_up() {
            eprintln!("Devnet is down, skipping test");
            return;
        }
    };
}
pub use skip_if_devnet_down;
