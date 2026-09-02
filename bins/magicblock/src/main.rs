mod domain;
mod healthcheck;

use std::io;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "magicblock", about = "MagicBlock operator tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the Magic Domain Program record for a leader.
    Domain(domain::Args),
    /// Check a validator's RPC, execution, and subscription paths.
    Healthcheck(healthcheck::Args),
}

impl Command {
    async fn run(self) -> Result<()> {
        match self {
            Self::Domain(args) => args.run().await,
            Self::Healthcheck(args) => args.run().await,
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_tracing();
    Cli::parse().command.run().await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .try_init();
}
