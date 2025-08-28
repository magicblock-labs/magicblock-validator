mod prelude {
    pub(super) use crate::{
        error::RpcError,
        requests::{params::Serde32Bytes, JsonWsRequest as JsonRequest},
        server::websocket::dispatch::{SubResult, WsDispatcher},
        RpcResult,
    };
}

pub(crate) mod account_subscribe;
pub(crate) mod log_subscribe;
pub(crate) mod program_subscribe;
pub(crate) mod signature_subscribe;
pub(crate) mod slot_subscribe;
