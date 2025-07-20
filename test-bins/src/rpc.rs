mod shutdown;

use log::*;
use magicblock_api::{
    ledger,
    magic_validator::{MagicValidator, MagicValidatorConfig},
    InitGeyserServiceConfig,
};
use magicblock_config::{GeyserGrpcConfig, MagicBlockConfig};
use solana_sdk::signature::Signer;
use test_tools::init_logger;

use crate::shutdown::Shutdown;

const GIT_VERSION: &str = git_version::git_version!();

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

    let mb_config = MagicBlockConfig::parse_config();

    match &mb_config.config_file {
        Some(file) => info!("Loading config from '{:?}'.", file),
        None => info!("Using default config. Override it by passing the path to a config file."),
    };

    info!("Starting validator with config:\n{}", mb_config.config);

    // Add a more developer-friendly startup message
    const WS_PORT_OFFSET: u16 = 1;
    let rpc_port = mb_config.config.rpc.port;
    let ws_port = rpc_port + WS_PORT_OFFSET; // WebSocket port is typically RPC port + 1
    let rpc_host = mb_config.config.rpc.addr;

    let validator_keypair = mb_config.validator_keypair();
    info!("Validator identity: {}", validator_keypair.pubkey());

    let geyser_grpc_config = mb_config.config.geyser_grpc.clone();
    let init_geyser_service_config =
        init_geyser_config(&mb_config, geyser_grpc_config);
    let config = MagicValidatorConfig {
        validator_config: mb_config.config,
        init_geyser_service_config,
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
    info!(
        "🏷️ Validator version: {} (Git: {})",
        env!("CARGO_PKG_VERSION"),
        GIT_VERSION
    );
    info!("-----------------------------------");
    info!("📡 RPC endpoint:       http://{}:{}", rpc_host, rpc_port);
    info!("🔌 WebSocket endpoint: ws://{}:{}", rpc_host, ws_port);
    info!("-----------------------------------");
    info!("Ready for connections!");
    info!("");

    if let Err(err) = Shutdown::wait().await {
        error!("Failed to gracefully shutdown: {}", err);
    }
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

fn init_geyser_config(
    mb_config: &MagicBlockConfig,
    grpc_config: GeyserGrpcConfig,
) -> InitGeyserServiceConfig {
    let (cache_accounts, cache_transactions) = {
        let cache_accounts =
            mb_config.geyser_cache_disable.contains("accounts");
        let cache_transactions =
            mb_config.geyser_cache_disable.contains("transactions");
        (cache_accounts, cache_transactions)
    };
    let (enable_account_notifications, enable_transaction_notifications) = {
        let enable_accounts = mb_config.geyser_disable.contains("accounts");
        let enable_transactions =
            mb_config.geyser_disable.contains("transactions");
        (enable_accounts, enable_transactions)
    };

    InitGeyserServiceConfig {
        cache_accounts,
        cache_transactions,
        enable_account_notifications,
        enable_transaction_notifications,
        geyser_grpc: grpc_config,
    }
}
