use error::RpcError;

mod encoder;
pub mod error;
mod requests;
pub mod server;
mod state;
mod utils;

type RpcResult<T> = Result<T, RpcError>;
type Slot = u64;
