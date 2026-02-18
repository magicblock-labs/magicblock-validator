use clap::Parser;
use magicblock_tui_client::{enrich_config_from_rpc, run_tui, TuiConfig};

#[derive(Debug, Parser)]
#[command(name = "magicblock-tui-client")]
#[command(about = "External TUI for Magicblock validator over RPC/WS")]
struct Args {
    /// HTTP JSON-RPC endpoint
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc_url: String,

    /// WebSocket endpoint
    #[arg(long, default_value = "ws://127.0.0.1:8900")]
    ws_url: String,

    /// Optional remote RPC endpoint to display in config tab
    #[arg(long, default_value = "")]
    remote_rpc_url: String,

    /// Optional validator identity override for config tab
    #[arg(long)]
    validator_identity: Option<String>,

    /// Optional ledger path for config tab
    #[arg(long, default_value = "")]
    ledger_path: String,

    /// Optional block time display value in ms
    #[arg(long, default_value_t = 400)]
    block_time_ms: u64,

    /// Optional lifecycle mode display value
    #[arg(long, default_value = "external")]
    lifecycle_mode: String,

    /// Optional base fee display value in lamports
    #[arg(long, default_value_t = 0)]
    base_fee: u64,

    /// Help URL shown in footer
    #[arg(long, default_value = "https://docs.magicblock.xyz")]
    help_url: String,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut config = TuiConfig {
        rpc_url: args.rpc_url,
        ws_url: args.ws_url,
        remote_rpc_url: args.remote_rpc_url,
        validator_identity: args.validator_identity.unwrap_or_default(),
        ledger_path: args.ledger_path,
        block_time_ms: args.block_time_ms,
        lifecycle_mode: args.lifecycle_mode,
        base_fee: args.base_fee,
        help_url: args.help_url,
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_version: option_env!("GIT_HASH").unwrap_or("unknown").to_string(),
    };

    enrich_config_from_rpc(&mut config).await;

    run_tui(config).await?;
    Ok(())
}
