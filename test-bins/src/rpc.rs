use log::*;
use sleipnir_api::{
    magic_validator::{MagicValidator, MagicValidatorConfig},
    InitGeyserServiceConfig,
};
use sleipnir_config::{GeyserGrpcConfig, SleipnirConfig};
use sleipnir_ledger::Ledger;
use solana_sdk::signature::Keypair;
use tempfile::TempDir;
use test_tools::init_logger;

// mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev
const TEST_KEYPAIR_BYTES: [u8; 64] = [
    7, 83, 184, 55, 200, 223, 238, 137, 166, 244, 107, 126, 189, 16, 194, 36,
    228, 68, 43, 143, 13, 91, 3, 81, 53, 253, 26, 36, 50, 198, 40, 159, 11, 80,
    9, 208, 183, 189, 108, 200, 89, 77, 168, 76, 233, 197, 132, 22, 21, 186,
    202, 240, 105, 168, 157, 64, 233, 249, 100, 104, 210, 41, 83, 87,
];

#[tokio::main]
async fn main() {
    init_logger!();

    #[cfg(feature = "tokio-console")]
    console_subscriber::init();

    let (file, config) = load_config_from_arg();
    match file {
        Some(file) => info!("Loading config from '{}'.", file),
        None => info!("Using default config. Override it by passing the path to a config file."),
    };
    info!("Starting validator with config:\n{}", config);

    let validator_keypair = validator_keypair();

    let ledger = {
        let ledger_path = TempDir::new().unwrap();
        Ledger::open(ledger_path.path())
            .expect("Expected to be able to open database ledger")
    };
    let geyser_grpc_config = config.geyser_grpc.clone();
    let config = MagicValidatorConfig {
        validator_config: config,
        ledger: Some(ledger),
        init_geyser_service_config: init_geyser_config(geyser_grpc_config),
    };

    debug!("{:#?}", config);
    let api = &mut MagicValidator::try_from_config(config, validator_keypair)
        .unwrap();
    debug!("Created API .. starting things up");
    api.start().await.expect("Failed to start validator");
    api.join();
}

fn validator_keypair() -> Keypair {
    // Try to load it from an env var base58 encoded private key
    if let Ok(keypair) = std::env::var("VALIDATOR_KEYPAIR") {
        Keypair::from_base58_string(&keypair)
    } else {
        warn!("Using default test keypair, provide one by setting 'VALIDATOR_KEYPAIR' env var to a base58 encoded private key");
        Keypair::from_bytes(&TEST_KEYPAIR_BYTES)
            // SAFETY: these bytes are compiled into the code, thus we know it is valid
            .unwrap()
    }
}

fn load_config_from_arg() -> (Option<String>, SleipnirConfig) {
    let config_file = std::env::args().nth(1);
    match config_file {
        Some(config_file) => {
            let config = SleipnirConfig::try_load_from_file(&config_file)
                .unwrap_or_else(|err| {
                    panic!(
                        "Failed to load config file from '{}'. ({})",
                        config_file, err
                    )
                });
            (Some(config_file), config)
        }
        None => (None, Default::default()),
    }
}

fn init_geyser_config(
    grpc_config: GeyserGrpcConfig,
) -> InitGeyserServiceConfig {
    let (cache_accounts, cache_transactions) =
        match std::env::var("GEYSER_CACHE_DISABLE") {
            Ok(val) => {
                let cache_accounts = !val.contains("accounts");
                let cache_transactions = !val.contains("transactions");
                (cache_accounts, cache_transactions)
            }
            Err(_) => (true, true),
        };
    let (enable_account_notifications, enable_transaction_notifications) =
        match std::env::var("GEYSER_DISABLE") {
            Ok(val) => {
                let enable_accounts = !val.contains("accounts");
                let enable_transactions = !val.contains("transactions");
                (enable_accounts, enable_transactions)
            }
            Err(_) => (true, true),
        };

    InitGeyserServiceConfig {
        cache_accounts,
        cache_transactions,
        enable_account_notifications,
        enable_transaction_notifications,
        geyser_grpc: grpc_config,
        ..Default::default()
    }
}
