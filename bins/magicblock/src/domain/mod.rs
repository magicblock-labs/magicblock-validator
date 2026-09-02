mod client;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Subcommand};
use isocountry::CountryCode as IsoCountryCode;
use magicblock_config::LeaderParams;
use mdp::state::{
    features::FeaturesSet,
    record::{CountryCode, ErRecord},
    status::ErStatus,
    version::v0::RecordV0,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

use self::client::Client;

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

impl Args {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Command::Register(args) => {
                let (operator, record) = args.load()?;
                operator.client.register(&operator.signer, record).await
            }
            Command::Sync(args) => {
                let (operator, record) = args.load()?;
                operator.client.sync(&operator.signer, &record).await
            }
            Command::Unregister(args) => {
                let operator = args.load()?;
                operator.client.unregister(&operator.signer).await
            }
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Create a domain record.
    Register(RecordArgs),
    /// Replace the mutable fields of an existing domain record.
    Sync(RecordArgs),
    /// Remove the leader's domain record.
    Unregister(ConfigArgs),
}

#[derive(ClapArgs)]
struct ConfigArgs {
    /// Leader configuration supplying RPC and signing identity.
    #[arg(long)]
    config: PathBuf,
}

impl ConfigArgs {
    fn load(self) -> Result<Operator> {
        Operator::load(self.config)
    }
}

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
    fn load(self) -> Result<(Operator, ErRecord)> {
        let Self {
            common,
            country_code,
            fqdn,
        } = self;
        let operator = common.load()?;
        let country = IsoCountryCode::for_alpha2_caseless(&country_code)
            .with_context(|| {
                format!("invalid ISO alpha-2 country code {country_code}")
            })?;
        let fqdn = url::Url::parse(&fqdn)
            .with_context(|| format!("invalid FQDN URL {fqdn}"))?;
        let block_time_ms = u16::try_from(operator.block_time.as_millis())
            .context(
                "leader block time exceeds the domain program u16 range",
            )?;
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

struct Operator {
    client: Client,
    signer: Arc<Keypair>,
    block_time: Duration,
}

impl Operator {
    fn load(path: PathBuf) -> Result<Self> {
        let config = LeaderParams::load(&path)
            .map_err(|error| anyhow::anyhow!(error.to_string()))
            .with_context(|| {
                format!("failed to load leader config {}", path.display())
            })?;
        Ok(Self {
            client: Client::new(config.rpc_url()),
            signer: config.engine.authority.local,
            block_time: config.engine.blockstore.blocktime,
        })
    }
}
