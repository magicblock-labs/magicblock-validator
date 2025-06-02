use std::net::IpAddr;

use clap::Parser;
use magicblock_api::EphemeralConfig;
use magicblock_config::{MetricsConfig, MetricsServiceConfig};

#[derive(Parser, Debug)]
pub struct MetricsConfigArgs {
    /// Whether to enable metrics.
    #[arg(long = "metrics-enabled", env = "METRICS_ENABLED")]
    pub enabled: Option<bool>,
    /// The interval in seconds at which system metrics are collected.
    #[arg(
        long = "metrics-system-metrics-tick-interval-secs",
        env = "METRICS_SYSTEM_METRICS_TICK_INTERVAL_SECS"
    )]
    pub system_metrics_tick_interval_secs: Option<u64>,
    /// The address to listen on
    #[arg(long = "metrics-addr", env = "METRICS_ADDR")]
    pub metrics_addr: Option<IpAddr>,
    /// The port to listen on
    #[arg(long = "metrics-port", env = "METRICS_PORT")]
    pub metrics_port: Option<u16>,
}

impl MetricsConfigArgs {
    pub fn merge_with_config(&self, config: &mut EphemeralConfig) {
        config.metrics = MetricsConfig {
            enabled: self.enabled.unwrap_or(config.metrics.enabled),
            system_metrics_tick_interval_secs: self
                .system_metrics_tick_interval_secs
                .unwrap_or(config.metrics.system_metrics_tick_interval_secs),
            service: MetricsServiceConfig {
                addr: self.metrics_addr.unwrap_or(config.metrics.service.addr),
                port: self.metrics_port.unwrap_or(config.metrics.service.port),
            },
        }
    }
}
