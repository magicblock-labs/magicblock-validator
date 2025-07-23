use error::RpcError;

mod encoder;
pub mod error;
mod notification;
mod requests;
pub mod server;
mod state;

type RpcResult<T> = Result<T, RpcError>;
