// NOTE: from rpc/src/rpc_service.rs :69

use jsonrpc_http_server::{hyper, RequestMiddleware, RequestMiddlewareAction};
use log::*;
pub(crate) struct RpcRequestMiddleware;

impl RequestMiddleware for RpcRequestMiddleware {
    fn on_request(
        &self,
        request: hyper::Request<hyper::Body>,
    ) -> RequestMiddlewareAction {
        trace!("request uri: {}", request.uri());
        request.into()
    }
}
