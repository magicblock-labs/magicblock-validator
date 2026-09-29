#![doc = include_str!("../README.md")]
#![recursion_limit = "256"]

pub mod metrics;
mod service;

pub use service::MetricsService;
