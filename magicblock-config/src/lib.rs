use std::{ffi::OsString, fmt::Display, path::PathBuf};

use clap::Parser;
use config::{
    aperture::ApertureConfig, cli::CliParams, metrics::MetricsConfig,
    LifecycleMode,
};
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
        CommittorConfig, CompressionConfig, LedgerConfig, LoadableProgram,
        TaskSchedulerConfig, ValidatorConfig,
    },
    types::Remote,
};

/// Top-level configuration, assembled from multiple sources.
#[derive(Clone, Deserialize, Serialize, Debug, Default)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatorParams {
    /// Path to the TOML configuration file (overrides CLI args).
    pub config: Option<PathBuf>,

    /// Remote endpoints for syncing with the base chain.
    /// Can include HTTP (for JSON-RPC), WebSocket (for PubSub), and gRPC (for streaming) connections.
    pub remotes: Vec<Remote>,

    /// The application's operational mode.
    pub lifecycle: LifecycleMode,

    /// Root directory for application storage.
    pub storage: StorageDirectory,

    /// Listen address for the metrics endpoint.
    pub metrics: MetricsConfig,

    /// Validator-specific arguments.
    pub validator: ValidatorConfig,

    /// Aperture-specific configuration.
    pub aperture: ApertureConfig,

    // --- File-Only Configuration ---
    pub commit: CommittorConfig,
    pub accountsdb: AccountsDbConfig,
    pub ledger: LedgerConfig,
    pub chainlink: ChainLinkConfig,
    pub chain_operation: Option<ChainOperationConfig>,
    pub task_scheduler: TaskSchedulerConfig,
    pub compression: CompressionConfig,
    pub programs: Vec<LoadableProgram>,
}

impl ValidatorParams {
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
    ) -> Result<Self, Box<figment::error::Error>> {
        // 1. Parse CLI arguments into the "Overlay" struct
        let cli = CliParams::parse_from(args);

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

        let mut params: Self = figment.extract().map_err(Box::new)?;
        params.ensure_http();
        params.ensure_websocket();
        Ok(params)
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
            .any(|r| matches!(r, Remote::Websocket(_)))
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
            .filter(|r| matches!(r, Remote::Websocket(_)))
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

impl Display for ValidatorParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match toml::to_string_pretty(self) {
            Ok(s) => f.write_str(&s),
            Err(_) => write!(f, "{:?}", self),
        }
    }
}
