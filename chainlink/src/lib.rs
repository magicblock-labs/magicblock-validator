#![doc = include_str!("../README.md")]
#![allow(deprecated)]
#![allow(clippy::result_large_err)]
pub mod chainlink;
pub mod cloner;
pub mod remote_account_provider;
pub mod submux;

pub use ::metrics::metrics::{AccountFetchContext, AccountFetchEntrypoint};
pub use chainlink::*;

#[cfg(any(test, feature = "dev-context"))]
pub mod testing;
