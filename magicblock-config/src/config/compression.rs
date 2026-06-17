use serde::{Deserialize, Serialize};

/// Configuration for the compression service.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct CompressionConfig {
    /// The URL of the Photon indexer.
    pub photon_url: Option<String>,
}
