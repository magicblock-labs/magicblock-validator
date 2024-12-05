// NOTE: from rpc/src/rpc_service.rs :69

use jsonrpc_http_server::{hyper, RequestMiddleware, RequestMiddlewareAction};
use log::*;

use crate::rpc_health::{RpcHealth, RpcHealthStatus};
pub(crate) struct RpcRequestMiddleware {
    health: RpcHealth,
}

impl RpcRequestMiddleware {
    pub fn new(health: RpcHealth) -> Self {
        Self { health }
    }

    fn health_check(&self) -> &'static str {
        let response = match self.health.check() {
            RpcHealthStatus::Ok => "ok",
            RpcHealthStatus::Unknown => "unknown",
        };
        info!("health check: {}", response);
        response
    }
}

impl RequestMiddleware for RpcRequestMiddleware {
    fn on_request(
        &self,
        request: hyper::Request<hyper::Body>,
    ) -> RequestMiddlewareAction {
        trace!("request uri: {}", request.uri());
        if request.uri().path() == "/health" {
            hyper::Response::builder()
                .status(hyper::StatusCode::OK)
                .body(hyper::Body::from(self.health_check()))
                .unwrap()
                .into()
        } else {
            request.into()
        }
    }
}
