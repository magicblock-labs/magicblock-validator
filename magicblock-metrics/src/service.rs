use std::net::SocketAddr;

use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::{
    body::Bytes, server::conn::http1, service::service_fn, Method, Request,
    Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use log::*;
use prometheus::TextEncoder;
use tokio::{net::TcpListener, select};
use tokio_util::sync::CancellationToken;

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
    runtime: tokio::runtime::Runtime,
    cancellation_token: CancellationToken,
}

impl MetricsService {
    fn try_new(
        addr: SocketAddr,
        cancellation_token: CancellationToken,
    ) -> std::io::Result<MetricsService> {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .thread_name("metrics-service")
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                error!("Failed to create metrics service runtime: {:?}", err);
                return Err(err);
            }
        };
        Ok(MetricsService {
            addr,
            cancellation_token,
            runtime,
        })
    }

    fn spawn(&self) {
        let addr = self.addr;
        let cancellation_token = self.cancellation_token.clone();
        self.runtime.spawn(Self::run(addr, cancellation_token));
    }

    async fn run(
        addr: SocketAddr,
        cancellation_token: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn(async move {
            let listener = match TcpListener::bind(&addr).await {
                Ok(listener) => {
                    info!("Serving {}/metrics", &addr);
                    listener
                }
                Err(err) => {
                    error!("Failed to bind to {}: {:?}", &addr, err);
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
                                        error!("Error: {:?}", err);
                                    }
                                });
                            }
                            Err(err) => error!(
                                "Accepting connection from {} failed: {:?}",
                                addr, err
                            ),
                        };
                    }
                );
            }
        })
    }
}

async fn metrics_service_router(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    if log_enabled!(log::Level::Trace) {
        trace!(
            "[{}] {:?} from {} ({})",
            req.method(),
            req.uri()
                .path_and_query()
                .map(|x| x.as_str())
                .unwrap_or_default(),
            req.headers()
                .get("host")
                .map(|h| h.to_str().unwrap_or_default())
                .unwrap_or_default(),
            req.headers()
                .get("user-agent")
                .map(|h| h.to_str().unwrap_or_default())
                .unwrap_or_default(),
        );
    }
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/metrics") => {
            let metrics = TextEncoder::new()
                .encode_to_string(&metrics::REGISTRY.gather())
                .unwrap_or_else(|error| {
                    warn!("could not encode custom metrics: {}", error);
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
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

fn empty() -> BoxBody<Bytes, hyper::Error> {
    let map_err = Empty::<Bytes>::new().map_err(|never| match never {});
    map_err.boxed()
}
