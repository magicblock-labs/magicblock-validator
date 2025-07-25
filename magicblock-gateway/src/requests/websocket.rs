use crate::{error::RpcError, requests::JsonRpcMethod, RpcResult};

use super::JsonRequest;
use utils::SubResult;

pub(crate) async fn dispatch(request: JsonRequest) -> RpcResult<SubResult> {
    use JsonRpcMethod::*;
    match request.method {
        AccountSubscribe => {}
        AccountUnsubscribe => {}
        ProgramSubscribe => {}
        ProgramUnsubscribe => {}
        SlotSubscribe => {}
        SlotUnsubsribe => {}
        LogsSubscribe => {}
        LogsUnsubscribe => {}
        unknown => return Err(RpcError::method_not_found(unknown)),
    }
    todo!()
}

mod account_subscribe;
mod log_subscribe;
mod program_subscribe;
mod slot_subscribe;
pub(crate) mod utils;
