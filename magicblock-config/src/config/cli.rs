use std::path::PathBuf;

use clap::{Args, Parser};
use serde::Serialize;

use crate::{
    config::LifecycleMode,
    types::{BindAddress, RemoteCluster, SerdeKeypair},
};

/// CLI Arguments mirroring the structure of ValidatorParams.
/// All fields are optional to allow "overlay" behavior on top of the config file.
#[derive(Parser, Serialize, Debug)]
#[command(author, version, about)]
pub struct CliParams {
    /// Path to the TOML configuration file.
    pub config: Option<PathBuf>,

    /// Remote Solana cluster URL or a predefined alias.
    #[arg(long, short)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteCluster>,

    /// The application's operational mode.
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleMode>,

    /// Root directory for application storage.
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<PathBuf>,

    /// Listen address for the metrics endpoint.
    #[arg(long, short)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<BindAddress>,

    /// Validator-specific arguments.
    #[command(flatten)]
    pub validator: CliValidatorConfig,

    /// Aperture-specific arguments.
    #[command(flatten)]
    pub aperture: CliApertureConfig,
}

/// CLI analog of configuration for the validator's core behavior and identity.
#[derive(Args, Serialize, Debug)]
pub struct CliValidatorConfig {
    /// Base fee in lamports for transactions.
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basefee: Option<u64>,

    /// The validator's identity keypair, encoded in Base58.
    #[arg(long, short)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keypair: Option<SerdeKeypair>,
}

/// CLI analog of configuration for Aperture functionality: RPC, Websocket, Geyser
#[derive(Args, Serialize, Debug)]
#[clap(rename_all = "kebab-case")]
pub struct CliApertureConfig {
    /// Primary listen address for the main RPC service.
    #[arg(long, short)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen: Option<BindAddress>,
    /// Number of event processor background task, these are responsible
    /// for syncing aperture state with the rest of the validator and
    /// propagating the updates to websocket and geyser subscribers
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_processors: Option<usize>,
}
