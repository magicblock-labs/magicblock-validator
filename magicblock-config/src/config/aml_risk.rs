use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::consts;

/// Configuration for checking account risk with the Range API and a local sqlite cache.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct AmlRiskConfig {
    /// Enables post-delegation address risk checks.
    pub enabled: bool,
    /// Base URL for the Range API.
    pub base_url: String,
    /// API key used for Range API authorization.
    pub api_key: Option<String>,
    /// TTL for cache entries.
    #[serde(with = "humantime")]
    pub cache_ttl: Duration,
    /// Request timeout for Range API calls.
    #[serde(with = "humantime")]
    pub request_timeout: Duration,
    /// Threshold on a scale of 0-10 for the risk score.
    pub risk_score_threshold: u64,
}

impl Default for AmlRiskConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: consts::DEFAULT_RISK_BASE_URL.to_string(),
            api_key: None,
            cache_ttl: Duration::from_secs(consts::DEFAULT_RISK_CACHE_TTL_SEC),
            request_timeout: Duration::from_secs(
                consts::DEFAULT_RISK_REQUEST_TIMEOUT_SEC,
            ),
            risk_score_threshold: consts::DEFAULT_RISK_SCORE_THRESHOLD,
        }
    }
}
