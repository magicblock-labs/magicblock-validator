use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::BindAddress;

/// Configuration for Aperture functionality: RPC, Websocket, Geyser
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct ApertureConfig {
    /// Primary listen/bind address for RPC service, websocket
    /// bind address is derived by incrementing the port by 1
    pub listen: BindAddress,
    /// Path list to the geyser plugin configuration files
    pub geyser_plugins: Vec<PathBuf>,
}
