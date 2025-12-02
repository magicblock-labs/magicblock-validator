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
        CommittorConfig, LedgerConfig, LoadableProgram, TaskSchedulerConfig,
        ValidatorConfig,
    },
    types::RemoteCluster,
};

/// Top-level configuration, assembled from multiple sources.
#[derive(Clone, Deserialize, Serialize, Debug, Default)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatorParams {
    /// Path to the TOML configuration file (overrides CLI args).
    pub config: Option<PathBuf>,

    /// Remote Solana cluster URL or a predefined alias.
    pub remote: RemoteCluster,

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
    pub programs: Vec<LoadableProgram>,
}

impl ValidatorParams {
    /// Assembles the final configuration.
    /// Precedence: CLI (if set) > Environment > TOML File > Defaults
    pub fn try_new(
        args: impl Iterator<Item = OsString>,
    ) -> figment::Result<Self> {
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
        figment.extract()
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
