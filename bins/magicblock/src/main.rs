#![doc = include_str!("../README.md")]

mod claim_fees;
mod domain;
mod healthcheck;
mod monitor;

use std::{io, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::LeaderParams;
use tracing_subscriber::EnvFilter;

/// Operator commands that act on existing services rather than starting a validator.
#[derive(Parser)]
#[command(name = "magicblock", about = "MagicBlock operator tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Actions against a validator's RPC or its base-chain state.
#[derive(Subcommand)]
enum Command {
    /// Claim accrued validator fees once on the base chain.
    ClaimFees(ConfigArgs),
    /// Manage the Magic Domain Program record for a leader.
    Domain(domain::Args),
    /// Check a validator's RPC, execution, and subscription paths.
    Healthcheck(healthcheck::Args),
    /// Passively log Engine transaction failures.
    Monitor(monitor::Args),
}

impl Command {
    /// Runs the selected action with failures propagated to the process exit status.
    async fn run(self) -> Result<()> {
        match self {
            Self::ClaimFees(args) => claim_fees::run(args.load()?).await,
            Self::Domain(args) => args.run().await,
            Self::Healthcheck(args) => args.run().await,
            Self::Monitor(args) => args.run().await,
        }
    }
}

/// Explicit leader configuration for commands that use its signing identity.
#[derive(clap::Args)]
struct ConfigArgs {
    /// Leader configuration supplying RPC and signing identity.
    #[arg(long)]
    config: PathBuf,
}

impl ConfigArgs {
    /// Requires the supplied file and applies the leader's defaults and environment overlay.
    fn load(self) -> Result<LeaderParams> {
        LeaderParams::load(&self.config)
            .with_context(|| format!("failed to load leader config {}", self.config.display()))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_tracing();
    Cli::parse().command.run().await
}

/// Routes operator diagnostics to stderr, with verbosity controlled by `RUST_LOG`.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(io::stderr)
        .try_init();
}
