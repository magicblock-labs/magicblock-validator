use integration_test_tools::validator::{
    start_test_validator_with_config, TestRunnerPaths,
};
use integration_test_tools::IntegrationTestContext;
use lazy_static::lazy_static;
use magicblock_api::domain_registry_manager::DomainRegistryManager;
use mdp::state::record::CountryCode;
use mdp::state::status::ErStatus;
use mdp::state::version::v0::RecordV0;
use mdp::state::{features::FeaturesSet, record::ErRecord};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::{Keypair, Signer};
use std::path::PathBuf;
use std::process::Child;
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
};

lazy_static! {
    static ref VALIDATOR_KEYPAIR: Arc<Keypair> = Arc::new(Keypair::new());
}

const DEVNET_URL: &str = "http://127.0.0.1:7799";

fn test_registration() {
    let validator_info = get_validator_info();
    let domain_manager = DomainRegistryManager::new(DEVNET_URL);
    domain_manager
        .handle_registration(&VALIDATOR_KEYPAIR, validator_info.clone())
        .expect("Failed to register");

    let actual = domain_manager
        .fetch_validator_info(&validator_info.pda().0)
        .expect("Failed to fetch ")
        .expect("ValidatorInfo doesn't exist");

    assert_eq!(actual, validator_info);
}

fn get_validator_info() -> ErRecord {
    ErRecord::V0(RecordV0 {
        identity: VALIDATOR_KEYPAIR.pubkey(),
        status: ErStatus::Active,
        block_time_ms: 101,
        base_fee: 102,
        features: FeaturesSet::default(),
        load_average: 222,
        country_code: CountryCode::from(
            isocountry::CountryCode::for_alpha2("BO").unwrap().alpha3(),
        ),
        addr: SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 0), 1010).to_string(),
    })
}

fn test_sync() {
    let mut validator_info = get_validator_info();
    match validator_info {
        ErRecord::V0(ref mut val) => {
            val.status = ErStatus::Draining;
            val.base_fee = 0;
        }
    }

    let domain_manager = DomainRegistryManager::new(DEVNET_URL);
    domain_manager
        .sync(&VALIDATOR_KEYPAIR, &validator_info)
        .expect("Failed to sync");

    let actual = domain_manager
        .fetch_validator_info(&validator_info.pda().0)
        .expect("Failed to fetch ")
        .expect("ValidatorInfo doesn't exist");

    assert_eq!(actual, validator_info);
}

fn test_unregister() {
    let domain_manager = DomainRegistryManager::new(DEVNET_URL);
    domain_manager
        .unregister(&VALIDATOR_KEYPAIR)
        .expect("Failed to unregister");

    let (pda, _) = DomainRegistryManager::get_pda(&VALIDATOR_KEYPAIR.pubkey());
    let actual = domain_manager
        .fetch_validator_info(&pda)
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

fn main() {
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
    test_registration();

    println!("Testing validator info sync...");
    test_sync();

    println!("Testing validator info unregistration...");
    test_unregister();

    println!("Passed")
}
