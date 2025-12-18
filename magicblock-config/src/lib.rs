//! A layered configuration library for the MagicBlock validator.

use std::{ffi::OsString, fmt::Display, path::PathBuf};

use clap::Parser;
use config::{cli::CliParams, metrics::MetricsConfig, LifecycleMode};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::Uncased,
    Figment, Profile,
};
use serde::{Deserialize, Serialize};
use types::StorageDirectory;

pub mod config;
pub mod consts;
#[cfg(test)]
mod tests;
pub mod types;

use crate::{
    config::{
        AccountsDbConfig, ChainLinkConfig, ChainOperationConfig,
        CommittorConfig, LedgerConfig, LoadableProgram, TaskSchedulerConfig,
        ValidatorConfig,
    },
    types::{resolve_url, BindAddress, RemoteConfig, RemoteKind},
};

/// Top-level configuration, assembled from multiple sources.
#[derive(Clone, Deserialize, Serialize, Debug, Default)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatorParams {
    /// Path to the TOML configuration file (overrides CLI args).
    pub config: Option<PathBuf>,

    /// Array-based remote configurations for RPC, WebSocket, and gRPC.
    /// Configured via [[remote]] sections in TOML (array-of-tables syntax).
    #[serde(default, rename = "remote")]
    pub remotes: Vec<RemoteConfig>,

    /// The application's operational mode.
    pub lifecycle: LifecycleMode,

    /// Root directory for application storage.
    pub storage: StorageDirectory,

    /// Primary listen address for the main RPC service.
    pub listen: BindAddress,

    /// Listen address for the metrics endpoint.
    pub metrics: MetricsConfig,

    /// Validator-specific arguments.
    pub validator: ValidatorConfig,

    // --- File-Only Configuration ---
    pub commit: CommittorConfig,
    pub accountsdb: AccountsDbConfig,
    pub ledger: LedgerConfig,
    pub chainlink: ChainLinkConfig,
    pub chain_operation: Option<ChainOperationConfig>,
    pub task_scheduler: TaskSchedulerConfig,
    pub programs: Vec<LoadableProgram>,
}

impl ValidatorParams {
    /// Assembles the final configuration.
    /// Precedence: CLI (if set) > Environment > TOML File > Defaults
    pub fn try_new(
        args: impl Iterator<Item = OsString>,
    ) -> Result<Self, Box<figment::error::Error>> {
        // 1. Parse CLI arguments into the "Overlay" struct
        let cli = CliParams::parse_from(args);

        // 2. Start with system defaults
        let mut figment = Figment::new()
            .merge(Serialized::defaults(ValidatorParams::default()));

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

        figment.extract().map_err(Box::new)
    }

    /// Returns the first RPC remote URL as an Option.
    pub fn rpc_url(&self) -> Option<&str> {
        self.remotes
            .iter()
            .find(|r| r.kind == RemoteKind::Rpc)
            .map(|r| r.url.as_str())
    }

    /// Returns an iterator over all WebSocket remote URLs.
    pub fn websocket_urls(&self) -> impl Iterator<Item = &str> + '_ {
        self.remotes
            .iter()
            .filter(|r| r.kind == RemoteKind::Websocket)
            .map(|r| r.url.as_str())
    }

    /// Returns an iterator over all gRPC remote URLs.
    pub fn grpc_urls(&self) -> impl Iterator<Item = &str> + '_ {
        self.remotes
            .iter()
            .filter(|r| r.kind == RemoteKind::Grpc)
            .map(|r| r.url.as_str())
    }

    pub fn has_subscription_url(&self) -> bool {
        self.remotes.iter().any(|r| {
            r.kind == RemoteKind::Websocket || r.kind == RemoteKind::Grpc
        })
    }

    /// Returns the RPC URL, using DEFAULT_REMOTE as fallback if not
    /// configured.
    pub fn rpc_url_or_default(&self) -> String {
        self.rpc_url().map(|s| s.to_string()).unwrap_or_else(|| {
            resolve_url(RemoteKind::Rpc, consts::DEFAULT_REMOTE)
        })
    }
}

impl Display for ValidatorParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match toml::to_string_pretty(self) {
            Ok(s) => f.write_str(&s),
            Err(_) => write!(f, "{:?}", self),
        }
    }
}
