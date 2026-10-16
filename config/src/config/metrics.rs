use serde::{Deserialize, Serialize};

use crate::{consts, types::BindAddress};

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct MetricsConfig {
    /// Address at which the system metrics are exposed
    pub address: BindAddress,
}

// The default bind address is a fixed, validated constant.
#[allow(clippy::unwrap_used)]
impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            address: consts::DEFAULT_METRICS_ADDR.parse().unwrap(),
        }
    }
}
