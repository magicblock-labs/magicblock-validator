//! A layered configuration library for the MagicBlock validator.

use clap::{Parser, ValueEnum};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment, Profile,
};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, path::PathBuf};

pub mod config;
pub mod consts;
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
#[derive(Parser, Deserialize, Serialize, Debug, Default)]
#[serde(default, rename_all = "kebab-case")]
#[command(author, version, about)]
pub struct MagicBlockParams {
    /// Path to the TOML configuration file (overrides CLI args).
    #[arg(long, short, global = true)]
    pub config: Option<PathBuf>,

    /// Remote Solana cluster URL or a predefined alias.
    #[arg(long, short, default_value = consts::DEFAULT_REMOTE)]
    pub remote: RemoteCluster,

    /// The application's operational mode.
    #[arg(long, value_enum, default_value = consts::DEFAULT_LIFECYCLE)]
    pub lifecycle: LifecycleMode,

    /// Root directory for application storage.
    #[arg(long)]
    pub storage: Option<PathBuf>,

    /// Primary listen address for the main RPC service.
    #[arg(long, short, default_value = consts::DEFAULT_RPC_ADDR)]
    pub listen: BindAddress,

    /// Listen address for the metrics endpoint.
    #[arg(long, short)]
    pub metrics: Option<BindAddress>,

    /// Validator-specific arguments.
    #[clap(flatten)]
    pub validator: ValidatorConfig,

    // --- File-Only Configuration ---
    #[clap(skip)]
    pub commit: CommitStrategy,
    #[clap(skip)]
    pub accounts_db: AccountsDbConfig,
    #[clap(skip)]
    pub ledger: LedgerConfig,
    #[clap(skip)]
    pub chainlink: ChainLinkConfig,
    #[clap(skip)]
    pub chain_operation: Option<ChainOperationConfig>,
    #[clap(skip)]
    pub task_scheduler: TaskSchedulerConfig,
    #[clap(skip)]
    pub programs: Vec<LoadableProgram>,
}

impl MagicBlockParams {
    pub fn try_new(
        args: impl Iterator<Item = OsString>,
    ) -> figment::Result<Self> {
        let cli = Self::parse_from(args);
        let mut figment = Figment::new().merge(Serialized::defaults(&cli));

        if let Some(path) = &cli.config {
            figment = figment.merge(Toml::file(path).profile(Profile::Default));
        }

        figment = figment.merge(
            Env::prefixed(consts::ENV_VAR_PREFIX)
                .split("_")
                .profile(Profile::Default),
        );

        figment.extract()
    }
}

#[derive(
    ValueEnum, Debug, Clone, Default, PartialEq, Deserialize, Serialize,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum LifecycleMode {
    #[default]
    Ephemeral,
    Replica,
    Offline,
    ProgramsReplica,
}
