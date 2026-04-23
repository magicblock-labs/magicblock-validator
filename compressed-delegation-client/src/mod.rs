//! Legacy client module layout (`accounts`, `instructions`, `types`).

pub mod accounts;
pub mod instructions;
pub mod programs;
pub mod shared;
pub mod types;

pub(crate) use programs::*;
