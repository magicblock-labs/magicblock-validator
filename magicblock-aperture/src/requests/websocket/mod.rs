use engine::Engine;

mod prelude {
    pub(super) use super::context_slot;
    pub(super) use crate::{
        RpcResult,
        requests::{JsonWsRequest as JsonRequest, params::Serde32Bytes},
        server::websocket::dispatch::{SubResult, WsDispatcher},
        state::subscriptions::next_subid,
    };
}

fn context_slot(engine: &Engine) -> u64 {
    engine.blocks().latest().slot
}

pub(crate) mod account_subscribe;
pub(crate) mod log_subscribe;
pub(crate) mod program_subscribe;
pub(crate) mod signature_subscribe;
pub(crate) mod slot_subscribe;
