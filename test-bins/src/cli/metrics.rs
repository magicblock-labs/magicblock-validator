use std::net::IpAddr;

use clap::Parser;

#[derive(Parser, Debug)]
pub struct MetricsConfigArgs {
    /// Whether to enable metrics.
    #[arg(
        long = "metrics-enabled",
        default_value = "true",
        env = "METRICS_ENABLED"
    )]
    pub enabled: bool,
    /// The interval in seconds at which system metrics are collected.
    #[arg(
        long = "metrics-system-metrics-tick-interval-secs",
        default_value = "30",
        env = "METRICS_SYSTEM_METRICS_TICK_INTERVAL_SECS"
    )]
    pub system_metrics_tick_interval_secs: u64,
    /// The address to listen on
    #[arg(
        long = "metrics-addr",
        default_value = "0.0.0.0",
        env = "METRICS_ADDR"
    )]
    pub metrics_addr: IpAddr,
    /// The port to listen on
    #[arg(long = "metrics-port", default_value = "9000", env = "METRICS_PORT")]
    pub metrics_port: u16,
}
