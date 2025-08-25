use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
};

use integration_test_tools::IntegrationTestContext;
use lazy_static::lazy_static;
use magicblock_api::domain_registry_manager::DomainRegistryManager;
use mdp::state::{
    features::FeaturesSet,
    record::{CountryCode, ErRecord},
    status::ErStatus,
    version::v0::RecordV0,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signer},
};

lazy_static! {
    static ref VALIDATOR_KEYPAIR: Arc<Keypair> = Arc::new(Keypair::new());
}

const DEVNET_URL: &str = "http://localhost:7799";

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

fn test_registration() {
    let validator_info = get_validator_info();
    let domain_manager = DomainRegistryManager::new(DEVNET_URL);
    domain_manager
        .handle_registration(&VALIDATOR_KEYPAIR, validator_info.clone())
        .expect("Failed to register");

    let actual = domain_manager
        .fetch_validator_info(&validator_info.pda().0)
        .expect("Failed to fetch validator info");

    assert_eq!(actual, Some(validator_info.clone()));
}

fn test_sync() {
    let mut validator_info = get_validator_info();
    match validator_info {
        ErRecord::V0(ref mut val) => {
            val.status = ErStatus::Draining;
            val.base_fee = 0;
            val.addr = "this.is.very.long.string.to.test.sync".to_string();
        }
    }

    let domain_manager = DomainRegistryManager::new(DEVNET_URL);
    domain_manager
        .sync(&VALIDATOR_KEYPAIR, &validator_info)
        .expect("Failed to sync");

    let actual = domain_manager
        .fetch_validator_info(&validator_info.pda().0)
        .expect("Failed to fetch validator info");

    assert_eq!(actual, Some(validator_info.clone()));
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

    assert!(actual.is_none());
}

#[test]
fn test_domain_registry() {
    let client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );
    IntegrationTestContext::airdrop(
        &client,
        &VALIDATOR_KEYPAIR.pubkey(),
        LAMPORTS_PER_SOL,
        CommitmentConfig::confirmed(),
    )
    .expect("Airdrop failed");

    test_registration();
    test_sync();
    test_unregister();
}
