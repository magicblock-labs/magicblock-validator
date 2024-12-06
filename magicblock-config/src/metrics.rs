use serde::{Deserialize, Serialize};

use crate::helpers;

helpers::socket_addr_config! {
    MetricsServiceConfig,
    9_000,
    "metrics_service"
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct MetricsConfig {
    #[serde(default = "helpers::serde_defaults::bool_true")]
    pub enabled: bool,
    #[serde(default = "default_system_metrics_tick_interval_secs")]
    pub system_metrics_tick_interval_secs: u64,
    #[serde(default)]
    #[serde(flatten)]
    pub service: MetricsServiceConfig,
}

fn default_system_metrics_tick_interval_secs() -> u64 {
    30
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            system_metrics_tick_interval_secs:
                default_system_metrics_tick_interval_secs(),
            service: Default::default(),
        }
    }
}
