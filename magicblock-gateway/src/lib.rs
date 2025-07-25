use error::RpcError;

pub mod error;
pub mod requests;
pub mod server;
pub mod state;

type RpcResult<T> = Result<T, RpcError>;
