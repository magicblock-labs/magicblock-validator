#![recursion_limit = "256"]

pub mod metrics;
mod service;

pub use service::{MetricsService, try_start_metrics_service};
