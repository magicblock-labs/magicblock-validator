use integration_test_tools::validator::{
    start_test_validator_with_config, wait_for_validator, TestRunnerPaths,
};
use integration_test_tools::IntegrationTestContext;
use lazy_static::lazy_static;
use magicblock_api::domain_registry_manager::DomainRegistryManager;
use mdp::state::{features::FeaturesSet, validator_info::ValidatorInfo};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::{Keypair, Signer};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
};

lazy_static! {
    static ref VALIDATOR_KEYPAIR: Arc<Keypair> = Arc::new(Keypair::new());
}

const DEVNET_URL: &str = "http://127.0.0.1:7799";

async fn test_registration() {
    let validator_info = ValidatorInfo {
        identity: VALIDATOR_KEYPAIR.pubkey(),
        addr: SocketAddrV4::new(Ipv4Addr::new(192, 1, 1, 0), 1010),
        block_time_ms: 101,
        fees: 0,
        features: FeaturesSet::default(),
    };

    let domain_manager = DomainRegistryManager::new(DEVNET_URL);
    domain_manager
        .handle_registration(&VALIDATOR_KEYPAIR, validator_info.clone())
        .await
        .expect("Failed to register");

    let actual = domain_manager
        .fetch_validator_info(&validator_info.pda().0)
        .await
        .expect("Failed to fetch ")
        .expect("ValidatorInfo doesn't exist");

    assert_eq!(actual, validator_info);
}

async fn test_sync() {
    let validator_info = ValidatorInfo {
        identity: VALIDATOR_KEYPAIR.pubkey(),
        addr: SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 0), 1010),
        block_time_ms: 101,
        fees: 0,
        features: FeaturesSet::default(),
    };

    let domain_manager = DomainRegistryManager::new(DEVNET_URL);
    domain_manager
        .sync(&VALIDATOR_KEYPAIR, &validator_info)
        .await
        .expect("Failed to sync");

    let actual = domain_manager
        .fetch_validator_info(&validator_info.pda().0)
        .await
        .expect("Failed to fetch ")
        .expect("ValidatorInfo doesn't exist");

    assert_eq!(actual, validator_info);
}

async fn test_unregister() {
    let domain_manager = DomainRegistryManager::new(DEVNET_URL);
    domain_manager
        .unregister(&VALIDATOR_KEYPAIR)
        .await
        .expect("Failed to unregister");

    let (pda, _) = DomainRegistryManager::get_pda(&VALIDATOR_KEYPAIR.pubkey());
    let actual = domain_manager
        .fetch_validator_info(&pda)
        .await
        .expect("Failed to fetch validator info");

    assert!(actual.is_none())
}

struct TestValidator {
    process: Child,
}

impl TestValidator {
    fn start() -> Self {
        let manifest_dir_raw = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let manifest_dir = PathBuf::from(&manifest_dir_raw);

        let config_path =
            manifest_dir.join("../configs/schedulecommit-conf.devnet.toml");
        let workspace_dir = manifest_dir.join("../");
        let root_dir = workspace_dir.join("../");

        let paths = TestRunnerPaths {
            config_path,
            root_dir,
            workspace_dir,
        };
        let process = start_test_validator_with_config(&paths, None, "CHAIN")
            .expect("Failed to start devnet process");

        Self { process }
    }
}

impl Drop for TestValidator {
    fn drop(&mut self) {
        self.process
            .kill()
            .expect("Failed to stop solana-test-validator");
        self.process
            .wait()
            .expect("Failed to wait for solana-test-validator");
    }
}

#[tokio::main]
async fn main() {
    let _devnet = TestValidator::start();

    let client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );
    IntegrationTestContext::airdrop(
        &client,
        &VALIDATOR_KEYPAIR.pubkey(),
        5000000000,
        CommitmentConfig::confirmed(),
    )
    .expect("Failed to airdrop");

    println!("Testing validator info registration...");
    test_registration().await;

    println!("Testing validator info sync...");
    test_sync().await;

    println!("Testing validator info unregistration...");
    test_unregister().await;

    println!("Passed")
}
