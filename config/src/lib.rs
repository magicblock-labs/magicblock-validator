#![doc = include_str!("../README.md")]

use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{Error as CliError, Parser};
use config::{
    EngineConfig, FollowerReplication, LeaderReplication, aperture::ApertureConfig, cli::CliParams,
    metrics::MetricsConfig,
};
use figment::{
    Error as FigmentError, Figment, Profile,
    providers::{Env, Format, Serialized, Toml},
    value::Uncased,
};
use humantime::re::humantime::format_duration;
use serde::{Deserialize, Serialize};
use solana_signer::Signer;
use url::Position;

pub mod config;
pub mod consts;
#[cfg(test)]
mod tests;
pub mod types;

use crate::config::{
    ChainSyncConfig, CommittorConfig, LedgerConfig, LoadableProgram, WebSocketProvider,
};
use crate::types::network::websocket_url;

const VERIFIER_ENV_VAR_PREFIX: &str = "MBV_VERIFIER_";

/// Failure while parsing process arguments or assembling configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Command-line parsing failed or requested display-only output.
    #[error(transparent)]
    Cli(#[from] CliError),
    /// Layered configuration could not be loaded or validated.
    #[error(transparent)]
    Config(#[from] Box<FigmentError>),
}

/// Leader configuration assembled from defaults, TOML, environment, and CLI.
#[derive(Clone, Deserialize, Serialize, Default)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct LeaderParams {
    /// Path to the TOML configuration file (overrides CLI args).
    pub config: Option<PathBuf>,

    /// Listen address for the metrics endpoint.
    pub metrics: MetricsConfig,

    /// Engine storage, execution, identity, and replication configuration.
    pub engine: EngineConfig<LeaderReplication>,

    /// Aperture-specific configuration.
    pub aperture: ApertureConfig,

    // --- File-Only Configuration ---
    pub commit: CommittorConfig,
    pub ledger: LedgerConfig,
    /// Provider capacities, lifecycle streams, and signer assessment for account synchronization.
    pub chainsync: ChainSyncConfig,
    pub programs: Vec<LoadableProgram>,
}

impl LeaderParams {
    /// Assembles the final configuration from multiple sources.
    ///
    /// Configuration is merged in the following precedence order (highest to lowest):
    /// 1. Command-line arguments
    /// 2. Environment variables (with `MBV_` prefix)
    /// 3. TOML configuration file
    /// 4. Serde defaults for each field
    ///
    /// After merging, automatic guarantees are enforced:
    /// - At least one HTTP endpoint is configured (for JSON-RPC calls)
    /// - At least one WebSocket endpoint is configured (for subscriptions)
    pub fn try_new(args: impl Iterator<Item = OsString>) -> Result<Self, ConfigError> {
        // 1. Parse CLI arguments into the "Overlay" struct
        let cli = CliParams::try_parse_from(args)?;

        // 2. Start with system defaults (Figment will use serde defaults for each field)
        let mut figment = Figment::new();

        // 3. Merge TOML File
        if let Some(path) = &cli.config {
            figment = figment.merge(Toml::file(path).profile(Profile::Default));
        }

        // 4. Merge Environment Variables
        figment = figment.merge(
            Env::prefixed(consts::ENV_VAR_PREFIX)
                .split("__")
                .map(|k| Uncased::new(k.as_str().replace('_', "-")))
                .profile(Profile::Default),
        );

        // 5. Merge CLI "Overlay" (Highest Priority)
        figment = figment.merge(Serialized::from(&cli, Profile::Default));

        let params: Self = figment.extract().map_err(Box::new)?;
        params.validate().map_err(Into::into)
    }

    /// Loads a leader config file with the same defaults and environment
    /// overlay used by the leader binary, but without a CLI overlay.
    /// The exact file must exist; parent directories are not searched.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Box<figment::error::Error>> {
        let figment = Figment::new()
            .merge(Toml::file_exact(path.as_ref()).profile(Profile::Default))
            .merge(
                Env::prefixed(consts::ENV_VAR_PREFIX)
                    .split("__")
                    .map(|k| Uncased::new(k.as_str().replace('_', "-")))
                    .profile(Profile::Default),
            );
        let params: Self = figment.extract().map_err(Box::new)?;
        params.validate()
    }

    fn validate(mut self) -> Result<Self, Box<FigmentError>> {
        if self.engine.authority.remote.is_some() {
            return Err(Box::new(FigmentError::from(
                "engine.authority.remote is reserved for follower validators",
            )));
        }
        self.ensure_providers();
        self.ensure_valid_aperture_listen()?;
        Ok(self)
    }

    fn ensure_valid_aperture_listen(&self) -> Result<(), Box<FigmentError>> {
        let port = self.aperture.listen.0.port();
        if port == u16::MAX {
            return Err(Box::new(FigmentError::from(format!(
                "aperture.listen port {port} is invalid: the WebSocket server \
                 binds to port + 1, which has no valid value at {}. Use a \
                 listen port <= {}.",
                u16::MAX,
                u16::MAX - 1,
            ))));
        }
        Ok(())
    }

    /// Fills omitted HTTP and WebSocket providers directly in the synchronization config.
    #[allow(clippy::unwrap_used)]
    fn ensure_providers(&mut self) {
        if self.chainsync.http.is_empty() {
            self.chainsync.http.push(consts::DEFAULT_REMOTE.parse().unwrap());
        }
        if self.chainsync.websocket.0.is_empty() {
            let url = websocket_url(self.chainsync.http[0].clone());
            self.chainsync.websocket.0.push(WebSocketProvider {
                url,
                max_connections: 4,
                subs_per_connection: 900,
            });
        }
    }

    /// Returns the first HTTP remote URL for JSON-RPC calls.
    /// Falls back to the default remote if none is configured.
    pub fn rpc_url(&self) -> &str {
        self.chainsync.http.first().map_or(consts::DEFAULT_REMOTE, |url| url.as_str())
    }

    /// Returns configured WebSocket URLs.
    pub fn websocket_urls(&self) -> impl Iterator<Item = &str> + '_ {
        self.chainsync.websocket.0.iter().map(|provider| provider.url.as_str())
    }
}

impl fmt::Display for LeaderParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let engine = &self.engine;
        let bytes = engine.ledger.size_limit;
        let (unit, scale) = match bytes {
            bytes if bytes >= 1 << 30 => ("GiB", 1 << 30),
            bytes if bytes >= 1 << 20 => ("MiB", 1 << 20),
            bytes if bytes >= 1 << 10 => ("KiB", 1 << 10),
            _ => ("B", 1),
        };
        let ledger_limit = bytes as f64 / scale as f64;
        let precision = if bytes.is_multiple_of(scale) { 0 } else { 2 };

        let blocktime = engine.blockstore.blocktime;
        let seal_slots = engine.blockstore.superblock;
        write!(
            f,
            "{{authority={} accounts_db={:?} ledger={:?} ledger_limit=\"{ledger_limit:.precision$} {unit}\" block_interval={blocktime:?}",
            engine.authority.local.pubkey(),
            engine.accountsdb.directory,
            engine.ledger.directory,
        )?;
        if seal_slots == 0 {
            f.write_str(" periodic_seals=disabled")?;
        } else {
            write!(f, " seal_every_slots={seal_slots}")?;
            let seconds = (blocktime.as_secs_f64() * seal_slots as f64).round();
            if let Ok(interval) = Duration::try_from_secs_f64(seconds)
                && !interval.is_zero()
            {
                write!(
                    f,
                    " seal_interval_estimate=\"{}\"",
                    format_duration(interval)
                )?;
            }
        }

        write!(
            f,
            " replication_bind={} allowed_followers={} metrics_bind={} geyser_plugins_configured={} risk_checks={} upstreams=",
            engine.replication.bind_address,
            engine.replication.allowed_followers.len(),
            self.metrics.address,
            self.aperture.geyser_plugins.len(),
            self.chainsync.aml.is_some(),
        )?;
        let mut upstreams = f.debug_list();
        let remotes = self.chainsync.http.iter().map(|url| ("HTTP", url))
            .chain(self.chainsync.websocket.0.iter().map(|provider| ("WS", &provider.url)))
            .chain(self.chainsync.grpc.iter().map(|stream| ("gRPC", &stream.endpoint)));
        for (protocol, url) in remotes {
            let host = &url[Position::BeforeHost..Position::AfterPort];
            upstreams.entry(&format_args!("{protocol}:{host}"));
        }
        upstreams.finish()?;

        if !self.programs.is_empty() {
            f.write_str(" configured_programs=")?;
            f.debug_list()
                .entries(self.programs.iter().map(|program| &program.id.0))
                .finish()?;
        }
        f.write_str("}")
    }
}

impl fmt::Debug for LeaderParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Minimal configuration for a bare replicated engine.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VerifierParams {
    /// Listen address for the metrics endpoint.
    pub metrics: MetricsConfig,
    /// Engine storage, local identity, and upstream replication settings.
    pub engine: EngineConfig<FollowerReplication>,
    /// Loader-v4 programs that must match the leader's startup image.
    #[serde(default)]
    pub programs: Vec<LoadableProgram>,
}

#[derive(Parser)]
#[command(name = "magicblock-verifier")]
struct VerifierCli {
    /// Path to the verifier TOML configuration.
    config: PathBuf,
}

impl VerifierParams {
    /// Loads a verifier config from TOML followed by `MBV_VERIFIER_` overrides.
    pub fn try_new(args: impl Iterator<Item = OsString>) -> Result<Self, ConfigError> {
        let cli = VerifierCli::try_parse_from(args)?;
        let figment =
            Figment::new().merge(Toml::file(&cli.config).profile(Profile::Default)).merge(
                Env::prefixed(VERIFIER_ENV_VAR_PREFIX)
                    .split("__")
                    .map(|k| Uncased::new(k.as_str().replace('_', "-")))
                    .profile(Profile::Default),
            );
        let mut params: Self = figment.extract().map_err(Box::new)?;
        if params.engine.authority.remote.is_some() {
            return Err(Box::new(FigmentError::from(
                "engine.authority.remote is derived from replication.upstream-authority",
            ))
            .into());
        }
        if params.engine.replication.upstream_address.port() == 0 {
            return Err(Box::new(FigmentError::from(
                "engine.replication.upstream-address must use a non-zero port",
            ))
            .into());
        }
        if params.engine.replication.upstream_authority.0 == Default::default() {
            return Err(Box::new(FigmentError::from(
                "engine.replication.upstream-authority is required",
            ))
            .into());
        }
        params.engine.authority.remote = Some(params.engine.replication.upstream_authority.0);
        Ok(params)
    }
}

impl fmt::Debug for VerifierParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifierParams")
            .field("metrics", &self.metrics)
            .field("engine", &self.engine)
            .field("programs", &self.programs.len())
            .finish()
    }
}
