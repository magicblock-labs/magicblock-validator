use magicblock_core::version::Version;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload};

impl RpcHandlers {
    pub(crate) fn get_version(&self, request: &JsonRequest) -> HandlerResult {
        ResponsePayload::encode_no_context(&request.id, Version::default())
    }
}
