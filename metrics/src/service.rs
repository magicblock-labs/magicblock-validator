use std::{net::SocketAddr, time::Duration};

use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode, body::Bytes, server::conn::http1, service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use prometheus::TextEncoder;
use tokio::{net::TcpListener, select, task::JoinSet, time};
use tracing::{instrument, *};

use crate::metrics;

const MAX_CONNECTIONS: usize = 8;
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_RETRY_DELAY: Duration = Duration::from_secs(1);

pub struct MetricsService {
    addr: SocketAddr,
    listener: TcpListener,
}

impl MetricsService {
    pub async fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        metrics::register();
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        Ok(Self { addr, listener })
    }

    /// Serves metrics until its shutdown tier is cancelled.
    #[instrument(skip_all, fields(addr = %self.addr))]
    pub async fn run(self, mut shutdown: ShutdownHandle) {
        let listener = self.listener;
        info!("Metrics server started");
        let mut connections = JoinSet::new();

        loop {
            select!(
                _ = shutdown.signalled() => {
                    break;
                }
                Some(result) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = result {
                        debug!(?error, "Metrics connection task failed");
                    }
                }
                result = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                    let (stream, _) = match result {
                        Ok(connection) => connection,
                        Err(error) => {
                            warn!(?error, "Failed to accept metrics connection");
                            select! {
                                _ = shutdown.signalled() => break,
                                _ = time::sleep(ACCEPT_RETRY_DELAY) => {}
                            }
                            continue;
                        }
                    };
                    connections.spawn(async move {
                        if let Err(error) = http1::Builder::new()
                            .timer(TokioTimer::new())
                            .header_read_timeout(HEADER_TIMEOUT)
                            .keep_alive(false)
                            .max_headers(32)
                            .max_buf_size(16 * 1024)
                            .serve_connection(TokioIo::new(stream), service_fn(metrics_service_router))
                            .await
                        {
                            debug!(?error, "Metrics connection closed");
                        }
                    });
                }
            );
        }

        connections.shutdown().await;
        info!("Metrics server shutdown");
        shutdown.terminate(ShutdownReason::Signalled);
    }
}

#[instrument(
    skip(req),
    fields(
        method = %req.method(),
        path = req.uri().path(),
        host = field::Empty,
        user_agent = field::Empty
    )
)]
async fn metrics_service_router(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    // Record optional headers
    if let Some(host) = req.headers().get("host").and_then(|h| h.to_str().ok()) {
        Span::current().record("host", host);
    }
    if let Some(ua) = req.headers().get("user-agent").and_then(|h| h.to_str().ok()) {
        Span::current().record("user_agent", ua);
    }

    match (req.method(), req.uri().path()) {
        (&Method::GET, "/metrics") => {
            let mut metric_families = metrics::REGISTRY.gather();
            metric_families.extend(prometheus::gather());
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
    }
}

fn full<T: Into<Bytes>>(chunk: T) -> BoxBody<Bytes, hyper::Error> {
    Full::new(chunk.into()).map_err(|never| match never {}).boxed()
}

fn empty() -> BoxBody<Bytes, hyper::Error> {
    let map_err = Empty::<Bytes>::new().map_err(|never| match never {});
    map_err.boxed()
}
