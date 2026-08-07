mod domain;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use domain::DomainClient;
use isocountry::CountryCode as IsoCountryCode;
use magicblock_config::LeaderParams;
use mdp::state::{
    features::FeaturesSet,
    record::{CountryCode, ErRecord},
    status::ErStatus,
    version::v0::RecordV0,
};
use solana_signer::Signer;

#[derive(Parser)]
#[command(name = "magicblock", about = "MagicBlock operator tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the Magic Domain Program record for a leader.
    Domain {
        #[command(subcommand)]
        command: DomainCommand,
    },
}

#[derive(Subcommand)]
enum DomainCommand {
    /// Create a domain record.
    Register(RecordArgs),
    /// Replace the mutable fields of an existing domain record.
    Sync(RecordArgs),
    /// Remove the leader's domain record.
    Unregister(ConfigArgs),
}

#[derive(Args)]
struct ConfigArgs {
    /// Leader configuration supplying RPC and signing identity.
    #[arg(long)]
    config: PathBuf,
}

#[derive(Args)]
struct RecordArgs {
    #[command(flatten)]
    common: ConfigArgs,
    /// ISO 3166-1 alpha-2 country code.
    #[arg(long)]
    country_code: String,
    /// Public URL at which the leader is reachable.
    #[arg(long)]
    fqdn: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Domain { command } => run_domain(command).await,
    }
}

async fn run_domain(command: DomainCommand) -> Result<()> {
    match command {
        DomainCommand::Register(args) => {
            let (config, record) = record(args)?;
            DomainClient::new(config.rpc_url())
                .register(&config.engine.authority.local, record)
                .await
        }
        DomainCommand::Sync(args) => {
            let (config, record) = record(args)?;
            DomainClient::new(config.rpc_url())
                .sync(&config.engine.authority.local, &record)
                .await
        }
        DomainCommand::Unregister(args) => {
            let config = load(args.config)?;
            DomainClient::new(config.rpc_url())
                .unregister(&config.engine.authority.local)
                .await
        }
    }
}

fn record(args: RecordArgs) -> Result<(LeaderParams, ErRecord)> {
    let config = load(args.common.config)?;
    let country = IsoCountryCode::for_alpha2_caseless(&args.country_code)
        .with_context(|| {
            format!("invalid ISO alpha-2 country code {}", args.country_code)
        })?;
    let fqdn = url::Url::parse(&args.fqdn)
        .with_context(|| format!("invalid FQDN URL {}", args.fqdn))?;
    let block_time_ms = u16::try_from(
        config.engine.blockstore.blocktime.as_millis(),
    )
    .context("leader block time exceeds the domain program u16 range")?;
    let identity = config.engine.authority.local.pubkey();
    let record = ErRecord::V0(RecordV0 {
        identity,
        status: ErStatus::Active,
        block_time_ms,
        base_fee: 0,
        features: FeaturesSet::default(),
        load_average: 0,
        country_code: CountryCode::from(country.alpha3()),
        addr: fqdn.to_string(),
    });
    Ok((config, record))
}

fn load(path: PathBuf) -> Result<LeaderParams> {
    LeaderParams::load(&path)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .with_context(|| {
            format!("failed to load leader config {}", path.display())
        })
}
