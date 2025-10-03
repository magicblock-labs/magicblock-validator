mod shutdown;

use log::*;
use magicblock_api::{
    ledger,
    magic_validator::{MagicValidator, MagicValidatorConfig},
};
use magicblock_config::MagicBlockConfig;
use solana_sdk::signature::Signer;
use tokio::runtime::Builder;

use crate::shutdown::Shutdown;

fn init_logger() {
    let mut builder = env_logger::builder();
    builder.format_timestamp_micros().is_test(false);

    if let Ok(style) = std::env::var("RUST_LOG_STYLE") {
        use std::io::Write;
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
    }
    let _ = builder.try_init().inspect_err(|err| {
        eprintln!("Failed to init logger: {}", err);
    });
}

fn main() {
    // We dedicate half of the threads to async runtime (where RPC and other
    // io/timer bound services are running), and the other half is allocated
    // for the execution runtime (transaction scheduler/executor threads)
    let workers = (num_cpus::get() / 2).max(1);
    let runtime = Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .thread_name("async-runtime")
        .build()
        .expect("failed to build async runtime");
    runtime.block_on(run());
}

async fn run() {
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

    let config = MagicValidatorConfig {
        validator_config: mb_config.config,
    };

    debug!("{:#?}", config);
    let mut api = MagicValidator::try_from_config(config, validator_keypair)
        .await
        .unwrap();
    debug!("Created API .. starting things up");

    // We need to create and hold on to the ledger lock here in order to keep the
    // underlying file locked while the app is running.
    // This prevents other processes from locking it until we exit.
    let mut ledger_lock = ledger::ledger_lockfile(api.ledger().ledger_path());
    let _ledger_write_guard =
        ledger::lock_ledger(api.ledger().ledger_path(), &mut ledger_lock);

    api.start().await.expect("Failed to start validator");
    let version = magicblock_version::Version::default();
    info!("");
    info!("🧙 Magicblock Validator is running!");
    info!(
        "🏷️ Validator version: {} (Git: {})",
        version, version.git_version
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
    api.stop().await;
}
