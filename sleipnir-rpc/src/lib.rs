use sleipnir_rpc_client_api::custom_error::RpcCustomError;

mod account_resolver;
mod accounts;
mod accounts_scan;
mod filters;
mod full;
mod json_rpc_request_processor;
pub mod json_rpc_service;
mod rpc_health;
mod rpc_request_middleware;
mod traits;
mod utils;

pub(crate) type RpcCustomResult<T> = std::result::Result<T, RpcCustomError>;
