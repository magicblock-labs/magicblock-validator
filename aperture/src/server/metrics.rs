use std::net::SocketAddr;

use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode, body::Bytes, server::conn::http1, service::service_fn,
};
use hyper_util::rt::TokioIo;
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use prometheus::TextEncoder;
use tokio::{net::TcpListener, select, task::JoinSet};
use tracing::{info, warn};

pub struct MetricsService {
    listener: TcpListener,
}

impl MetricsService {
    pub async fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self { listener })
    }

    /// Serves metrics until its shutdown tier is cancelled.
    pub async fn run(self, mut shutdown: ShutdownHandle) {
        let reason = match serve(self.listener, &shutdown).await {
            Err(error) => ShutdownReason::Error(Box::new(error)),
            Ok(()) if shutdown.requested() => ShutdownReason::Signalled,
            Ok(()) => ShutdownReason::Unexpected,
        };
        shutdown.terminate(reason);
    }
}

async fn serve(listener: TcpListener, shutdown: &ShutdownHandle) -> std::io::Result<()> {
    info!("Metrics server started");
    let mut connections = JoinSet::new();

    let result = loop {
        select!(
            _ = shutdown.signalled() => {
                break Ok(());
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
            result = listener.accept() => {
                let (stream, _) = match result {
                    Ok(connection) => connection,
                    Err(error) => break Err(error),
                };
                let io = TokioIo::new(stream);
                connections.spawn(async move {
                    let con = http1::Builder::new();
                    let _ = con.serve_connection(io, service_fn(metrics_service_router)).await;
                });
            }
        );
    };

    connections.shutdown().await;
    info!("Metrics server shutdown");
    result
}

async fn metrics_service_router(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    // Record optional headers
    let result = match (req.method(), req.uri().path()) {
        (&Method::GET, "/metrics") => {
            let metric_families = prometheus::gather();
            let metrics =
                TextEncoder::new().encode_to_string(&metric_families).unwrap_or_else(|error| {
                    warn!(error = %error, "Failed to encode metrics");
                    String::new()
                });
            Ok(Response::new(full(metrics)))
        }
        _ => {
            let mut not_found = Response::new(empty());
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    };
    // We must consume the body fully to keep the connection alive. We
    // iterate over all chunks and simply drop them. This prevents garbage
    // data of previous requests from being stuck in connection buffer.
    let mut body = req.into_body();
    while (body.frame().await).is_some() {}

    result
}

fn full<T: Into<Bytes>>(chunk: T) -> BoxBody<Bytes, hyper::Error> {
    Full::new(chunk.into()).map_err(|never| match never {}).boxed()
}

fn empty() -> BoxBody<Bytes, hyper::Error> {
    let map_err = Empty::<Bytes>::new().map_err(|never| match never {});
    map_err.boxed()
}
