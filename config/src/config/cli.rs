use std::path::PathBuf;

use clap::{Args, Parser};
use serde::Serialize;

use crate::types::{BindAddress, network::Remote};

/// CLI arguments mirroring the structure of [`crate::LeaderParams`].
/// All fields are optional to allow "overlay" behavior on top of the config file.
#[derive(Parser, Serialize, Debug)]
#[command(author, version, about)]
pub struct CliParams {
    /// Path to the TOML configuration file.
    pub config: Option<PathBuf>,

    /// List of remote endpoints for syncing with the base chain.
    /// Can be specified multiple times.
    ///
    /// SUPPORTED SCHEMES: http(s), ws(s), grpc(s)
    ///
    /// ALIASES: mainnet, devnet, testnet, localhost
    ///
    /// EXAMPLES:
    /// - `--remotes devnet`
    /// - `--remotes wss://devnet.solana.com`
    /// - `--remotes grpcs://grpc.example.com`
    ///
    /// DEFAULT: devnet (HTTP endpoint with auto-added WS endpoint)
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remotes: Option<Vec<Remote>>,

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
    /// Number of event processor background task, these are responsible
    /// for syncing aperture state with the rest of the validator and
    /// propagating the updates to websocket and geyser subscribers
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_processors: Option<usize>,
}
