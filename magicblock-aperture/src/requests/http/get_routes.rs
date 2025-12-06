use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getRoutes` RPC request.
    ///
    /// This is a validator-specific, mocked implementation used only to satisfy
    /// the Magic Router / SDK API. A validator does not act as a router and
    /// therefore always returns an empty list of routes.
    pub(crate) fn get_routes(&self, request: &JsonRequest) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            json::json!([]),
        ))
    }
}
