use clap::Args;
use magicblock_config_macro::{clap_from_serde, clap_prefix};
use serde::{Deserialize, Serialize};

use crate::helpers;

helpers::socket_addr_config! {
    MetricsServiceConfig,
    9_000,
    "metrics_service",
    "metrics"
}

#[clap_prefix("metrics")]
#[clap_from_serde]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args)]
pub struct MetricsConfig {
    #[derive_env_var]
    #[arg(help = "Whether to enable metrics.")]
    #[serde(default = "helpers::serde_defaults::bool_true")]
    pub enabled: bool,
    #[derive_env_var]
    #[arg(
        help = "The interval at which system metrics should be collected in seconds."
    )]
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
