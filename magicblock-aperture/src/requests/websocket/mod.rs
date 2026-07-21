mod prelude {
    pub(super) use crate::{
        RpcResult,
        encoder::Encoder,
        requests::{JsonWsRequest as JsonRequest, params::Serde32Bytes},
        server::websocket::dispatch::{SubResult, WsDispatcher},
        state::subscriptions::next_subid,
    };
}

pub(crate) mod account_subscribe;
pub(crate) mod log_subscribe;
pub(crate) mod program_subscribe;
pub(crate) mod signature_subscribe;
pub(crate) mod slot_subscribe;
