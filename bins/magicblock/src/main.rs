#![doc = include_str!("../README.md")]

mod claim_fees;
mod domain;
mod healthcheck;

use std::{io, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use magicblock_config::LeaderParams;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "magicblock", about = "MagicBlock operator tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Claim accrued validator fees once on the base chain.
    ClaimFees(ConfigArgs),
    /// Manage the Magic Domain Program record for a leader.
    Domain(domain::Args),
    /// Check a validator's RPC, execution, and subscription paths.
    Healthcheck(healthcheck::Args),
}

impl Command {
    async fn run(self) -> Result<()> {
        match self {
            Self::ClaimFees(args) => claim_fees::run(args.load()?).await,
            Self::Domain(args) => args.run().await,
            Self::Healthcheck(args) => args.run().await,
        }
    }
}

#[derive(clap::Args)]
struct ConfigArgs {
    /// Leader configuration supplying RPC and signing identity.
    #[arg(long)]
    config: PathBuf,
}

impl ConfigArgs {
    fn load(self) -> Result<LeaderParams> {
        LeaderParams::load(&self.config).with_context(|| {
            format!("failed to load leader config {}", self.config.display())
        })
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
