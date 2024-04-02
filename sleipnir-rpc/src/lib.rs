use sleipnir_rpc_client_api::custom_error::RpcCustomError;

mod account_resolver;
mod filters;
mod handlers;
pub mod json_rpc_request_processor;
pub mod json_rpc_service;
mod rpc_health;
mod rpc_request_middleware;
mod traits;
mod transaction;
pub mod transaction_notifier_interface;
mod utils;

pub(crate) type RpcCustomResult<T> = std::result::Result<T, RpcCustomError>;

#[macro_use]
extern crate solana_metrics;
