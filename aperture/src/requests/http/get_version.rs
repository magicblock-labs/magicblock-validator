use magicblock_core::version::Version;

use super::HandlerResult;
use crate::{
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) fn get_version(&self, request: &JsonRequest) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            Version::default(),
        ))
    }
}
