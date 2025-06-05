use clap::Parser;
use log::*;
use magicblock_api::{
    ledger,
    magic_validator::{MagicValidator, MagicValidatorConfig},
    InitGeyserServiceConfig,
};
use magicblock_config::GeyserGrpcConfig;
use solana_sdk::signature::Signer;
use test_tools::init_logger;

mod cli;
use cli::*;

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

    let cli = Cli::parse();
    let config = match cli.get_ephemeral_config() {
        Ok(config) => config,
        Err(err) => {
            error!("Failed to parse config: {err}");
            return;
        }
    };

    info!("Starting validator with config:\n{}", config);
    // Add a more developer-friendly startup message
    const WS_PORT_OFFSET: u16 = 1;
    let rpc_port = config.rpc.port;
    let ws_port = rpc_port + WS_PORT_OFFSET; // WebSocket port is typically RPC port + 1
    let rpc_host = config.rpc.addr;

    info!("Validator identity: {}", cli.keypair.0.pubkey());

    let geyser_grpc_config = config.geyser_grpc.clone();
    let config = MagicValidatorConfig {
        validator_config: config,
        init_geyser_service_config: init_geyser_config(geyser_grpc_config),
    };

    debug!("{:#?}", config);
    let mut api = match MagicValidator::try_from_config(config, cli.keypair.0) {
        Ok(api) => api,
        Err(e) => {
            error!("Failed to create MagicValidator: {e}");
            std::process::exit(1);
        }
    };
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
