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

impl MetricsConfig {
    pub fn merge(&mut self, other: MetricsConfig) {
        if self.enabled == helpers::serde_defaults::bool_true()
            && other.enabled != helpers::serde_defaults::bool_true()
        {
            self.enabled = other.enabled;
        }
        if self.system_metrics_tick_interval_secs
            == default_system_metrics_tick_interval_secs()
            && other.system_metrics_tick_interval_secs
                != default_system_metrics_tick_interval_secs()
        {
            self.system_metrics_tick_interval_secs =
                other.system_metrics_tick_interval_secs;
        }
        self.service.merge(other.service);
    }
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

fn default_system_metrics_tick_interval_secs() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn test_merge_with_default() {
        let mut config = MetricsConfig {
            enabled: true,
            system_metrics_tick_interval_secs: 30000,
            service: MetricsServiceConfig {
                addr: IpAddr::V4(Ipv4Addr::new(1, 0, 0, 127)),
                port: 9091,
            },
        };
        let original_config = config.clone();
        let other = MetricsConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_merge_default_with_non_default() {
        let mut config = MetricsConfig::default();
        let other = MetricsConfig {
            enabled: true,
            system_metrics_tick_interval_secs: 30000,
            service: MetricsServiceConfig {
                addr: IpAddr::V4(Ipv4Addr::new(1, 0, 0, 127)),
                port: 9091,
            },
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_merge_non_default() {
        let mut config = MetricsConfig {
            enabled: false,
            system_metrics_tick_interval_secs: 30000,
            service: MetricsServiceConfig {
                addr: IpAddr::V4(Ipv4Addr::new(1, 0, 0, 127)),
                port: 9091,
            },
        };
        let original_config = config.clone();
        let other = MetricsConfig {
            enabled: true,
            system_metrics_tick_interval_secs: 60,
            service: MetricsServiceConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                port: 9090,
            },
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }
}
