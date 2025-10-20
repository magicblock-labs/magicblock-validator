mod shutdown;

use log::*;
use magicblock_api::{
    ledger,
    magic_validator::{MagicValidator, MagicValidatorConfig},
};
use magicblock_config::MagicBlockConfig;
use solana_sdk::signature::Signer;

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

/// Print informational startup messages.
/// - If `RUST_LOG` is not set or is set to "quiet", prints to stdout using `println!()`.
/// - Otherwise, emits an `info!` log so operators can control visibility
///   (e.g., by setting `RUST_LOG=warn` to hide it).
fn print_info<S: std::fmt::Display>(msg: S) {
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    let rust_log_trimmed = rust_log.trim().to_ascii_lowercase();
    let use_plain_print =
        rust_log_trimmed.is_empty() || rust_log_trimmed == "quiet";
    if use_plain_print {
        println!("{}", msg);
    } else {
        info!("{}", msg);
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
    let validator_identity = validator_keypair.pubkey();

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
    print_info("");
    print_info("🧙 Magicblock Validator is running! 🪄✦");
    print_info(format!(
        "🏷️ Validator version: {} (Git: {})",
        version, version.git_version
    ));
    print_info("-----------------------------------");
    print_info(format!(
        "📡 RPC endpoint:       http://{}:{}",
        rpc_host, rpc_port
    ));
    print_info(format!(
        "🔌 WebSocket endpoint: ws://{}:{}",
        rpc_host, ws_port
    ));
    print_info(format!("🖥️ Validator identity: {}", validator_identity));
    print_info(format!(
        "🗄️ Ledger location:    {}",
        api.ledger().ledger_path().to_str().unwrap_or("")
    ));
    print_info("-----------------------------------");
    print_info("Ready for connections!");
    print_info("");

    if let Err(err) = Shutdown::wait().await {
        error!("Failed to gracefully shutdown: {}", err);
    }
    api.stop().await;
}
