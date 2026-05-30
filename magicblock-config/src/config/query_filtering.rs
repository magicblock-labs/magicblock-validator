use serde::{Deserialize, Serialize};

use crate::consts;

/// Configuration for local query filtering in Aperture.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct QueryFilteringConfig {
    /// Whether Aperture should apply local permission-aware response filtering.
    pub enabled: bool,
    /// JWT secret used by the local query filtering login route.
    pub jwt_secret: String,
    /// Token expiration time in days.
    pub token_expiry_days: i64,
    /// Challenge expiration time in seconds.
    pub challenge_ttl_seconds: i64,
}

impl Default for QueryFilteringConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            jwt_secret: consts::DEFAULT_JWT_SECRET.to_string(),
            token_expiry_days: consts::DEFAULT_TOKEN_EXPIRY_DAYS,
            challenge_ttl_seconds: consts::DEFAULT_CHALLENGE_TTL_SECONDS,
        }
    }
}
