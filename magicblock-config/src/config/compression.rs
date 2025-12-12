use serde::{Deserialize, Serialize};

/// Configuration for the compression service.
#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct CompressionConfig {
    /// The URL of the Photon indexer.
    pub photon_url: Option<String>,
    /// The API key for the Photon indexer.
    pub api_key: Option<String>,
}
