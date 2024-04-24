use sleipnir_rpc_client_api::custom_error::RpcCustomError;

mod account_resolver;
mod filters;
mod handlers;
pub mod json_rpc_request_processor;
pub mod json_rpc_service;
mod perf;
mod rpc_health;
mod rpc_request_middleware;
mod traits;
mod transaction;
mod utils;

pub(crate) type RpcCustomResult<T> = std::result::Result<T, RpcCustomError>;

#[macro_use]
extern crate solana_metrics;
