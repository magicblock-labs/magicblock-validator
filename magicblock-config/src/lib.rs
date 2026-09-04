use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};

use clap::{Error as CliError, Parser};
use config::{
    EngineConfig, FollowerReplication, LeaderReplication,
    aperture::ApertureConfig, cli::CliParams, grpc::GrpcConfig,
    metrics::MetricsConfig,
};
use figment::{
    Error as FigmentError, Figment, Profile,
    providers::{Env, Format, Serialized, Toml},
    value::Uncased,
};
use serde::{Deserialize, Serialize};
use solana_signer::Signer;

pub mod config;
pub mod consts;
#[cfg(test)]
mod tests;
pub mod types;

use crate::{
    config::{
        AdminConfig, ChainLinkConfig, CommittorConfig, LedgerConfig,
        LoadableProgram, TaskSchedulerConfig,
    },
    types::Remote,
};

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

    /// Remote endpoints for syncing with the base chain.
    /// Can include HTTP (for JSON-RPC), WebSocket (for PubSub), and gRPC (for streaming) connections.
    pub remotes: Vec<Remote>,

    /// Listen address for the metrics endpoint.
    pub metrics: MetricsConfig,

    /// Global configuration for gRPC-based providers.
    pub grpc: GrpcConfig,

    /// Engine storage, execution, identity, and replication configuration.
    pub engine: EngineConfig<LeaderReplication>,

    /// Aperture-specific configuration.
    pub aperture: ApertureConfig,

    // --- File-Only Configuration ---
    pub commit: CommittorConfig,
    pub ledger: LedgerConfig,
    pub chainlink: ChainLinkConfig,
    pub admin: Option<AdminConfig>,
    pub task_scheduler: TaskSchedulerConfig,
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
    pub fn try_new(
        args: impl Iterator<Item = OsString>,
    ) -> Result<Self, ConfigError> {
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
    pub fn load(
        path: impl AsRef<Path>,
    ) -> Result<Self, Box<figment::error::Error>> {
        let figment = Figment::new()
            .merge(Toml::file(path.as_ref()).profile(Profile::Default))
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
        self.ensure_http();
        self.ensure_websocket();
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

    /// Ensures at least one HTTP endpoint is configured.
    /// If no HTTP remote is present, adds the default HTTP remote (devnet).
    fn ensure_http(&mut self) {
        let mut remotes = self.remotes.iter();
        if remotes.any(|r| matches!(r, Remote::Http(_))) {
            return;
        }
        self.remotes
            .push(Remote::Http(consts::DEFAULT_REMOTE.parse().unwrap()));
    }

    /// Ensures at least one WebSocket endpoint is configured.
    /// If no WebSocket remote is present, derives one from the first HTTP remote.
    /// This satisfies the requirement for a subscription-capable endpoint.
    fn ensure_websocket(&mut self) {
        // Check if a websocket remote already exists
        if self
            .remotes
            .iter()
            .any(|r| matches!(r, Remote::Websocket(..)))
        {
            return;
        }

        // Find the first HTTP remote and convert it to WebSocket
        if let Some(websocket) = self
            .remotes
            .iter()
            .find(|r| matches!(r, Remote::Http(_)))
            .and_then(|r| r.to_websocket())
        {
            self.remotes.push(websocket);
        } else {
            // Fallback: if no HTTP remote exists (unexpected, since ensure_http() was called first),
            // create a default WebSocket remote from the default HTTP remote.
            let default_http =
                Remote::Http(consts::DEFAULT_REMOTE.parse().unwrap());
            if let Some(default_websocket) = default_http.to_websocket() {
                self.remotes.push(default_websocket);
            }
        }
    }

    /// Returns the first HTTP remote URL for JSON-RPC calls.
    /// Falls back to the default remote if none is configured.
    pub fn rpc_url(&self) -> &str {
        self.remotes
            .iter()
            .find_map(|r| matches!(r, Remote::Http(_)).then(|| r.url_str()))
            .unwrap_or(consts::DEFAULT_REMOTE)
    }

    /// Returns an iterator over all WebSocket remote URLs.
    pub fn websocket_urls(&self) -> impl Iterator<Item = &str> + '_ {
        self.remotes
            .iter()
            .filter(|r| matches!(r, Remote::Websocket(..)))
            .map(|r| r.url_str())
    }

    /// Returns an iterator over all gRPC remote URLs for streaming subscriptions.
    pub fn grpc_urls(&self) -> impl Iterator<Item = &str> + '_ {
        self.remotes
            .iter()
            .filter(|r| matches!(r, Remote::Grpc(_)))
            .map(|r| r.url_str())
    }
}

impl fmt::Display for LeaderParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (mut http, mut websocket, mut grpc) = (0, 0, 0);
        for remote in &self.remotes {
            match remote {
                Remote::Http(_) => http += 1,
                Remote::Websocket(..) => websocket += 1,
                Remote::Grpc(_) => grpc += 1,
            }
        }
        let rows = [
            ("Role", "leader".to_owned()),
            (
                "Authority",
                self.engine.authority.local.pubkey().to_string(),
            ),
            (
                "AccountsDB",
                format!(
                    "{} (LRU {})",
                    self.engine.accountsdb.directory.display(),
                    self.engine.accountsdb.lru_capacity,
                ),
            ),
            (
                "Ledger",
                format!(
                    "{} (limit {})",
                    self.engine.ledger.directory.display(),
                    self.engine.ledger.size_limit,
                ),
            ),
            (
                "Blocks",
                format!(
                    "{:?}; superblock {}",
                    self.engine.blockstore.blocktime,
                    self.engine.blockstore.superblock,
                ),
            ),
            (
                "Replication",
                format!(
                    "{} ({} followers)",
                    self.engine.replication.bind_address,
                    self.engine.replication.allowed_followers.len(),
                ),
            ),
            (
                "RPC",
                format!(
                    "{} ({} workers, {} Geyser plugins)",
                    self.aperture.listen,
                    self.aperture.event_processors,
                    self.aperture.geyser_plugins.len(),
                ),
            ),
            (
                "Metrics",
                format!(
                    "{} every {:?}",
                    self.metrics.address, self.metrics.collect_frequency,
                ),
            ),
            (
                "Remotes",
                format!("HTTP {http}; WS {websocket}; gRPC {grpc}"),
            ),
            (
                "Chainlink",
                format!(
                    "risk {}; resubscribe {:?}",
                    if self.chainlink.risk.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    self.chainlink.resubscription_delay,
                ),
            ),
            (
                "Services",
                format!(
                    "{} programs; admin {}; TUI external",
                    self.programs.len(),
                    if self.admin.is_some() {
                        "enabled"
                    } else {
                        "disabled"
                    },
                ),
            ),
        ];
        let key_width = rows
            .iter()
            .map(|(key, _)| key.chars().count())
            .max()
            .unwrap_or_default()
            .max("Setting".len());
        let value_width = rows
            .iter()
            .map(|(_, value)| value.chars().count())
            .max()
            .unwrap_or_default()
            .max("Value".len());
        let horizontal = |width| "─".repeat(width + 2);

        writeln!(f, "┌{}┬{}┐", horizontal(key_width), horizontal(value_width))?;
        writeln!(f, "│ {:key_width$} │ {:value_width$} │", "Setting", "Value")?;
        writeln!(f, "├{}┼{}┤", horizontal(key_width), horizontal(value_width))?;
        for (key, value) in rows {
            writeln!(f, "│ {key:key_width$} │ {value:value_width$} │")?;
        }
        write!(f, "└{}┴{}┘", horizontal(key_width), horizontal(value_width))
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
    pub fn try_new(
        args: impl Iterator<Item = OsString>,
    ) -> Result<Self, ConfigError> {
        let cli = VerifierCli::try_parse_from(args)?;
        let figment = Figment::new()
            .merge(Toml::file(&cli.config).profile(Profile::Default))
            .merge(
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
        if params.engine.replication.upstream_authority.0 == Default::default()
        {
            return Err(Box::new(FigmentError::from(
                "engine.replication.upstream-authority is required",
            ))
            .into());
        }
        params.engine.authority.remote =
            Some(params.engine.replication.upstream_authority.0);
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
