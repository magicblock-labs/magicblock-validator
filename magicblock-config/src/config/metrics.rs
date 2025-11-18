use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{consts, types::BindAddress};

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct MetricsConfig {
    /// Address at which the system metrics are exposed
    pub address: BindAddress,
    /// Time frequency with which the metrics are collected
    #[serde(with = "humantime")]
    pub collect_frequency: Duration,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            address: consts::DEFAULT_METRICS_ADDR.parse().unwrap(),
            collect_frequency: Duration::from_secs(
                consts::DEFAULT_METRICS_COLLECT_FREQUENCY_SEC,
            ),
        }
    }
}
