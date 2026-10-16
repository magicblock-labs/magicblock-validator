use std::path::PathBuf;

use clap::{Args, Parser};
use serde::Serialize;

use crate::types::BindAddress;

/// CLI arguments mirroring the structure of [`crate::LeaderParams`].
/// All fields are optional to allow "overlay" behavior on top of the config file.
#[derive(Parser, Serialize, Debug)]
#[command(author, version, about)]
pub struct CliParams {
    /// Path to the TOML configuration file.
    pub config: Option<PathBuf>,

    /// Listen address for the metrics endpoint.
    #[arg(long, short)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<BindAddress>,

    /// Aperture-specific arguments
    #[command(flatten)]
    pub aperture: CliApertureConfig,
}

/// CLI analog of configuration for Aperture functionality: RPC, Websocket, Geyser
#[derive(Args, Serialize, Debug)]
#[clap(rename_all = "kebab-case")]
pub struct CliApertureConfig {
    /// Primary listen address for the main RPC service.
    #[arg(long, short)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen: Option<BindAddress>,
}
