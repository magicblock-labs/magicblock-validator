use clap::Args;
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};

#[clap_prefix("compression")]
#[clap_from_serde]
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args, Mergeable,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CompressionConfig {
    #[derive_env_var]
    #[arg(help = "The URL of the Photon indexer.")]
    #[serde(default = "default_photon_url")]
    pub photon_url: String,
    #[derive_env_var]
    #[clap_from_serde_skip] // Skip because it defaults to None
    #[arg(help = "The API key for the Photon indexer.")]
    #[serde(default = "default_api_key")]
    pub api_key: Option<String>,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            photon_url: default_photon_url(),
            api_key: default_api_key(),
        }
    }
}

fn default_photon_url() -> String {
    "http://localhost:8784".to_string()
}

fn default_api_key() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use magicblock_config_helpers::Merge;

    use super::*;

    #[test]
    fn test_merge_with_default() {
        let mut config = CompressionConfig {
            photon_url: "http://localhost:8785".to_string(),
            api_key: Some("api_key".to_string()),
        };
        let original_config = config.clone();
        let other = CompressionConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_merge_default_with_non_default() {
        let mut config = CompressionConfig::default();
        let other = CompressionConfig {
            photon_url: "http://localhost:8785".to_string(),
            api_key: Some("api_key".to_string()),
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_merge_non_default() {
        let mut config = CompressionConfig {
            photon_url: "http://localhost:8786".to_string(),
            api_key: Some("api_key".to_string()),
        };
        let original_config = config.clone();
        let other = CompressionConfig {
            photon_url: "http://localhost:8787".to_string(),
            api_key: Some("api_key".to_string()),
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }
}
