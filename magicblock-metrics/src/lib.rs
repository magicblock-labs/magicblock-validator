#![recursion_limit = "256"]

pub mod metrics;
mod service;

pub use service::{try_start_metrics_service, MetricsService};
