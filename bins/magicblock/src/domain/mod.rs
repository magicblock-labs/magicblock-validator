//! Base-chain domain records built from the leader identity and explicit operator inputs.

mod client;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Subcommand};
use config::LeaderParams;
use isocountry::CountryCode as IsoCountryCode;
use mdp::state::{
    features::FeaturesSet,
    record::{CountryCode, ErRecord},
    status::ErStatus,
    version::v0::RecordV0,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

use self::client::Client;
use crate::ConfigArgs;

/// Domain operation to perform using the supplied leader configuration.
#[derive(ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Command,
}

impl Args {
    /// Binds each operation's record identity and transaction payer to the same leader.
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            Command::Register(args) => {
                let (operator, record) = args.load()?;
                operator.client.register(&operator.signer, record).await
            }
            Command::Sync(args) => {
                let (operator, record) = args.load()?;
                operator.client.sync(&operator.signer, record).await
            }
            Command::Unregister(args) => {
                let operator = Operator::new(args.load()?);
                operator.client.unregister(&operator.signer).await
            }
        }
    }
}

/// Distinct on-chain create, update, and remove operations; none is an upsert.
#[derive(Subcommand)]
enum Command {
    /// Create a domain record.
    Register(RecordArgs),
    /// Update an existing domain record.
    Sync(RecordArgs),
    /// Remove the leader's domain record.
    Unregister(ConfigArgs),
}

/// Desired record inputs shared by creation and synchronization.
#[derive(ClapArgs)]
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

impl RecordArgs {
    /// Builds the desired V0 record, using defaults for fields not exposed as CLI options.
    /// Synchronization submits those defaults too; it does not merge with a fetched record.
    fn load(self) -> Result<(Operator, ErRecord)> {
        let Self { common, country_code, fqdn } = self;
        let operator = Operator::new(common.load()?);
        let country = IsoCountryCode::for_alpha2_caseless(&country_code)
            .with_context(|| format!("invalid ISO alpha-2 country code {country_code}"))?;
        let fqdn = url::Url::parse(&fqdn).with_context(|| format!("invalid FQDN URL {fqdn}"))?;
        let block_time_ms = u16::try_from(operator.block_time.as_millis())
            .context("leader block time exceeds the domain program u16 range")?;
        let record = ErRecord::V0(RecordV0 {
            identity: operator.signer.pubkey(),
            status: ErStatus::Active,
            block_time_ms,
            base_fee: 0,
            features: FeaturesSet::default(),
            load_average: 0,
            country_code: CountryCode::from(country.alpha3()),
            addr: fqdn.to_string(),
        });
        Ok((operator, record))
    }
}

/// Keeps record identity, signing authority, and timing tied to one leader configuration.
struct Operator {
    client: Client,
    signer: Arc<Keypair>,
    block_time: Duration,
}

impl Operator {
    /// Uses the leader configuration for both RPC access and record ownership.
    fn new(config: LeaderParams) -> Self {
        Self {
            client: Client::new(config.rpc_url()),
            signer: config.engine.authority.local,
            block_time: config.engine.blockstore.blocktime,
        }
    }
}
