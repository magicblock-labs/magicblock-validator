//! A layered configuration library for the MagicBlock validator.

use clap::Parser;
use config::{cli::CliParams, LifecycleMode};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment, Profile,
};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, path::PathBuf};

pub mod config;
pub mod consts;
#[cfg(test)]
mod tests;
pub mod types;

use crate::{
    config::{
        AccountsDbConfig, ChainLinkConfig, ChainOperationConfig,
        CommitStrategy, LedgerConfig, LoadableProgram, TaskSchedulerConfig,
        ValidatorConfig,
    },
    types::{BindAddress, RemoteCluster},
};

/// Top-level configuration, assembled from multiple sources.
#[derive(Deserialize, Serialize, Debug, Default)]
#[serde(default, rename_all = "kebab-case")]
pub struct MagicBlockParams {
    /// Path to the TOML configuration file (overrides CLI args).
    pub config: Option<PathBuf>,

    /// Remote Solana cluster URL or a predefined alias.
    pub remote: RemoteCluster,

    /// The application's operational mode.
    pub lifecycle: LifecycleMode,

    /// Root directory for application storage.
    pub storage: Option<PathBuf>,

    /// Primary listen address for the main RPC service.
    pub listen: BindAddress,

    /// Listen address for the metrics endpoint.
    pub metrics: Option<BindAddress>,

    /// Validator-specific arguments.
    pub validator: ValidatorConfig,

    // --- File-Only Configuration ---
    pub commit: CommitStrategy,
    pub accountsdb: AccountsDbConfig,
    pub ledger: LedgerConfig,
    pub chainlink: ChainLinkConfig,
    pub chain_operation: Option<ChainOperationConfig>,
    pub task_scheduler: TaskSchedulerConfig,
    pub programs: Vec<LoadableProgram>,
}

impl MagicBlockParams {
    /// Assembles the final configuration.
    /// Precedence: CLI (if set) > Environment > TOML File > Defaults
    pub fn try_new(
        args: impl Iterator<Item = OsString>,
    ) -> figment::Result<Self> {
        // 1. Parse CLI arguments into the "Overlay" struct
        let cli = CliParams::parse_from(args);

        // 2. Start with system defaults
        let mut figment = Figment::new()
            .merge(Serialized::defaults(MagicBlockParams::default()));

        // 3. Merge TOML File
        // Check CLI first for config path, fall back to a default path if you have one
        if let Some(path) = &cli.config {
            figment = figment.merge(Toml::file(path).profile(Profile::Default));
        }

        // 4. Merge Environment Variables
        figment = figment.merge(
            Env::prefixed(consts::ENV_VAR_PREFIX)
                .split("_")
                .profile(Profile::Default),
        );

        // 5. Merge CLI "Overlay" (Highest Priority)
        figment = figment.merge(Serialized::from(&cli, Profile::Default));

        // 6. Extract and Validate
        let params: Self = figment.extract()?;

        Ok(params)
    }
}
