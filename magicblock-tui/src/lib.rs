//! Terminal User Interface for MagicBlock Validator
//!
//! This crate provides a TUI for monitoring the validator's status,
//! viewing logs, transactions, and configuration in real-time.

mod app;
mod events;
mod logger;
mod state;
mod ui;

pub use app::run_tui;
pub use logger::TuiTracingLayer;
pub use state::{TuiConfig, ValidatorConfig};
