use std::net::SocketAddr;

use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::{
    body::Bytes, server::conn::http1, service::service_fn, Method, Request,
    Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use prometheus::TextEncoder;
use tokio::{net::TcpListener, select};
use tokio_util::sync::CancellationToken;
use tracing::{instrument, *};

use crate::metrics;

pub fn try_start_metrics_service(
    addr: SocketAddr,
    cancellation_token: CancellationToken,
) -> std::io::Result<MetricsService> {
    metrics::register();
    let service = MetricsService::try_new(addr, cancellation_token)?;
    service.spawn();
    Ok(service)
}

pub struct MetricsService {
    addr: SocketAddr,
    cancellation_token: CancellationToken,
}

impl MetricsService {
    fn try_new(
        addr: SocketAddr,
        cancellation_token: CancellationToken,
    ) -> std::io::Result<MetricsService> {
        Ok(MetricsService {
            addr,
            cancellation_token,
        })
    }

    fn spawn(&self) {
        let addr = self.addr;
        let cancellation_token = self.cancellation_token.clone();
        tokio::spawn(async move {
            Self::run(addr, cancellation_token).await;
        });
    }

    async fn run(addr: SocketAddr, cancellation_token: CancellationToken) {
        start_metrics_server(addr, cancellation_token).await;
    }
}

#[instrument(skip(cancellation_token), fields(addr = %addr))]
async fn start_metrics_server(
    addr: SocketAddr,
    cancellation_token: CancellationToken,
) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => {
            info!("Metrics server started");
            listener
        }
        Err(err) => {
            error!(error = ?err, "Failed to bind");
            return;
        }
    };

    loop {
        select!(
            _ = cancellation_token.cancelled() => {
                break;
            }
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let io = TokioIo::new(stream);
                        tokio::task::spawn(async move {
                            if let Err(err) = http1::Builder::new()
                            .serve_connection(io, service_fn(metrics_service_router))
                            .await
                        {
                            debug!(error = ?err, "Metrics connection closed");
                        }
                        });
                    }
                    Err(err) => error!(
                        error = ?err,
                        "Failed to accept connection"
                    ),
                };
            }
        );
    }

    info!("Metrics server shutdown");
}

#[instrument(
    skip(req),
    fields(
        method = %req.method(),
        path = req.uri().path(),
        host = tracing::field::Empty,
        user_agent = tracing::field::Empty
    )
)]
async fn metrics_service_router(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    // Record optional headers
    if let Some(host) = req.headers().get("host").and_then(|h| h.to_str().ok())
    {
        tracing::Span::current().record("host", host);
    }
    if let Some(ua) = req
        .headers()
        .get("user-agent")
        .and_then(|h| h.to_str().ok())
    {
        tracing::Span::current().record("user_agent", ua);
    }

    let result = match (req.method(), req.uri().path()) {
        (&Method::GET, "/metrics") => {
            let metrics = TextEncoder::new()
                .encode_to_string(&metrics::REGISTRY.gather())
                .unwrap_or_else(|error| {
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
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

fn empty() -> BoxBody<Bytes, hyper::Error> {
    let map_err = Empty::<Bytes>::new().map_err(|never| match never {});
    map_err.boxed()
}
