use log::*;
use magicblock_api::{
    ledger,
    magic_validator::{MagicValidator, MagicValidatorConfig},
    InitGeyserServiceConfig,
};
use magicblock_config::{EphemeralConfig, GeyserGrpcConfig};
use solana_sdk::signature::{Keypair, Signer};
use test_tools::init_logger;
use git_version::git_version;

// mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev
const TEST_KEYPAIR_BYTES: [u8; 64] = [
    7, 83, 184, 55, 200, 223, 238, 137, 166, 244, 107, 126, 189, 16, 194, 36,
    228, 68, 43, 143, 13, 91, 3, 81, 53, 253, 26, 36, 50, 198, 40, 159, 11, 80,
    9, 208, 183, 189, 108, 200, 89, 77, 168, 76, 233, 197, 132, 22, 21, 186,
    202, 240, 105, 168, 157, 64, 233, 249, 100, 104, 210, 41, 83, 87,
];

const GIT_VERSION: &str = git_version!();

fn init_logger() {
    if let Ok(style) = std::env::var("RUST_LOG_STYLE") {
        use std::io::Write;
        let mut builder = env_logger::builder();
        builder.format_timestamp_micros().is_test(false);
        match style.as_str() {
            "EPHEM" => {
                builder.format(|buf, record| {
                    writeln!(
                        buf,
                        "EPHEM [{}] {}: {} {}",
                        record.level(),
                        buf.timestamp_millis(),
                        record.module_path().unwrap_or_default(),
                        record.args()
                    )
                });
            }
            "DEVNET" => {
                builder.format(|buf, record| {
                    writeln!(
                        buf,
                        "DEVNET [{}] {}: {} {}",
                        record.level(),
                        buf.timestamp_millis(),
                        record.module_path().unwrap_or_default(),
                        record.args()
                    )
                });
            }
            _ => {}
        }
        let _ = builder.try_init();
    } else {
        init_logger!();
    }
}

#[tokio::main]
async fn main() {
    init_logger();
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();

    let (file, config) = load_config_from_arg();
    let config = config.override_from_envs();
    match file {
        Some(file) => info!("Loading config from '{}'.", file),
        None => info!("Using default config. Override it by passing the path to a config file."),
    };
    info!("Starting validator with config:\n{}", config);
    // Add a more developer-friendly startup message
    const WS_PORT_OFFSET: u16 = 1;
    let rpc_port = config.rpc.port;
    let ws_port = rpc_port + WS_PORT_OFFSET; // WebSocket port is typically RPC port + 1
    let rpc_host = config.rpc.addr;

    let validator_keypair = validator_keypair();

    info!("Validator identity: {}", validator_keypair.pubkey());

    let geyser_grpc_config = config.geyser_grpc.clone();
    let config = MagicValidatorConfig {
        validator_config: config,
        init_geyser_service_config: init_geyser_config(geyser_grpc_config),
    };

    debug!("{:#?}", config);
    let mut api =
        MagicValidator::try_from_config(config, validator_keypair).unwrap();
    debug!("Created API .. starting things up");

    // We need to create and hold on to the ledger lock here in order to keep the
    // underlying file locked while the app is running.
    // This prevents other processes from locking it until we exit.
    let mut ledger_lock = ledger::ledger_lockfile(api.ledger().ledger_path());
    let _ledger_write_guard =
        ledger::lock_ledger(api.ledger().ledger_path(), &mut ledger_lock);

    api.start().await.expect("Failed to start validator");

    info!("");
    info!("🧙 Magicblock Validator is running!");
    info!("Validator version: {} (Git: {})", env!("CARGO_PKG_VERSION"), GIT_VERSION);
    info!("-----------------------------------");
    info!("📡 RPC endpoint:       http://{}:{}", rpc_host, rpc_port);
    info!("🔌 WebSocket endpoint: ws://{}:{}", rpc_host, ws_port);
    info!("-----------------------------------");
    info!("Ready for connections!");
    info!("");

    // validator is supposed to run forever, so we wait for
    // termination signal to initiate a graceful shutdown
    let _ = tokio::signal::ctrl_c().await;

    info!("SIGTERM has been received, initiating graceful shutdown");
    // weird panic behavior in json rpc http server, which panics when stopped from
    // within async context, so we just move it to a different thread for shutdown
    //
    // TODO: once we move rpc out of the validator, this hack will be gone
    let _ = std::thread::spawn(move || {
        api.stop();
        api.join();
    })
    .join();
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

fn load_config_from_arg() -> (Option<String>, EphemeralConfig) {
    let config_file = std::env::args().nth(1);
    match config_file {
        Some(config_file) => {
            let config = EphemeralConfig::try_load_from_file(&config_file)
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
    }
}
