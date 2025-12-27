use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::BindAddress;

/// Configuration for Aperture functionality: RPC, Websocket, Geyser
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct ApertureConfig {
    /// Primary listen/bind address for RPC service, websocket
    /// bind address is derived by incrementing the port by 1
    pub listen: BindAddress,
    /// Number of event processor background task, these are responsible
    /// for syncing aperture state with the rest of the validator and for
    /// propagating the updates to the websocket and geyser subscribers
    pub event_processors: usize,
    /// Path list to the geyser plugin configuration files
    pub geyser_plugins: Vec<PathBuf>,
}

impl Default for ApertureConfig {
    fn default() -> Self {
        Self {
            listen: BindAddress::default(),
            event_processors: 1,
            geyser_plugins: Default::default(),
        }
    }
}
