use serde::{Deserialize, Serialize};

use crate::consts;

/// Configuration for the compression service.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct CompressionConfig {
    /// The URL of the Photon indexer.
    pub photon_url: String,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            photon_url: consts::DEFAULT_PHOTON_URL.to_string(),
        }
    }
}
