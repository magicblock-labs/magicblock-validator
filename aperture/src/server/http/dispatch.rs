use crate::{
    RpcResult,
    error::RpcError,
    requests::{
        RpcRequest,
        http::RpcHandlers,
        payload::{JsonBody, ResponseErrorPayload},
    },
};
use futures::{StreamExt, stream::FuturesOrdered};
use http_body_util::BodyExt;
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::{
        ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
        ACCESS_CONTROL_MAX_AGE, CONTENT_TYPE, HeaderName, HeaderValue,
    },
};
use json::Value;
use std::{convert::Infallible, sync::Arc};
use tracing::error;

const REMOTE_ACCOUNT_CLAIMS_HEADER: HeaderName =
    HeaderName::from_static("x-mb-remote-account-claims");

/// Retains a single body chunk without copying; allocates only for fragmented bodies.
enum RequestBody {
    Empty,
    Single(Bytes),
    Multi(Vec<u8>),
}

impl RequestBody {
    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Single(data) => data.len(),
            Self::Multi(data) => data.len(),
        }
    }
}

/// Distinguishes a missing body from malformed single or batch JSON.
fn parse_body(body: RequestBody) -> RpcResult<RpcRequest> {
    let bytes = match &body {
        RequestBody::Empty => {
            return Err(RpcError::invalid_request("missing request body"));
        }
        RequestBody::Single(data) => data.as_ref(),
        RequestBody::Multi(data) => data.as_ref(),
    }
    .trim_ascii_start();
    if bytes.first() == Some(&b'{') {
        json::from_slice(bytes).map(RpcRequest::Single)
    } else {
        json::from_slice(bytes).map(RpcRequest::Multi)
    }
    .map_err(Into::into)
}

/// Collects data frames under the HTTP body limit, ignoring trailers.
async fn extract_bytes(request: Request<Incoming>) -> RpcResult<RequestBody> {
    const MAX_BODY_SIZE: usize = 1024 * 1024;
    let mut body = request.into_body();
    let mut data = RequestBody::Empty;
    while let Some(frame) = body.frame().await {
        let Ok(chunk) = frame?.into_data() else {
            continue;
        };
        // Reject oversized chunks before allocating or extending the body buffer.
        if data.len() + chunk.len() > MAX_BODY_SIZE {
            return Err(RpcError::invalid_request("request body exceed 1MiB limit"));
        }
        match &mut data {
            RequestBody::Empty => data = RequestBody::Single(chunk),
            RequestBody::Single(first) => {
                let mut buffer = Vec::with_capacity(first.len() + chunk.len());
                buffer.extend_from_slice(first);
                buffer.extend_from_slice(&chunk);
                data = RequestBody::Multi(buffer);
            }
            RequestBody::Multi(buffer) => buffer.extend_from_slice(&chunk),
        }
    }
    Ok(data)
}

pub(super) async fn dispatch(
    handlers: Arc<RpcHandlers>,
    request: Request<Incoming>,
) -> Result<Response<JsonBody>, Infallible> {
    if request.method() == Method::OPTIONS {
        let mut response = Response::new(JsonBody(Vec::new()));
        set_headers(&mut response, 0);
        return Ok(response);
    }
    let request = match extract_bytes(request).await.and_then(parse_body) {
        Ok(request) => request,
        Err(error) => {
            let mut response = error_response(None, error);
            set_headers(&mut response, 0);
            return Ok(response);
        }
    };
    let (mut response, claims) = match request {
        RpcRequest::Single(r) => {
            let mut claims = 0;
            let response = match handlers.process(&r, &mut claims).await {
                Ok(body) => Response::new(body),
                Err(error) => error_response(Some(&r.id), error),
            };
            (response, claims)
        }
        RpcRequest::Multi(requests) => {
            // Poll calls concurrently while assembling responses in request order.
            let mut jobs = FuturesOrdered::new();
            for r in requests {
                let handlers = handlers.clone();
                jobs.push_back(async move {
                    let mut claims = 0;
                    let result = handlers.process(&r, &mut claims).await;
                    (result, claims, r.id)
                });
            }
            let mut body = vec![b'['];
            let mut claims = 0;
            while let Some((result, count, id)) = jobs.next().await {
                // Claims remain observable even when a later operation fails.
                claims += count;
                if body.len() != 1 {
                    body.push(b',');
                }
                let payload =
                    result.unwrap_or_else(|error| error_response(Some(&id), error).into_body());
                body.extend_from_slice(&payload.0);
            }
            body.push(b']');
            (Response::new(JsonBody(body)), claims)
        }
    };
    set_headers(&mut response, claims);
    Ok(response)
}

/// Produces an error envelope, with an explicit fallback if serialization fails.
fn error_response(id: Option<&Value>, error: RpcError) -> Response<JsonBody> {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = ResponseErrorPayload::encode(id, error).unwrap_or_else(|error| {
        error!(?error, "failed to serialize RPC error");
        JsonBody(br#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"response serialization failed"},"id":null}"#.to_vec())
    });
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
}

/// Set CORS/Access control related headers (required by explorers/web apps)
/// and the custom header to count the number of remote account requests
fn set_headers(response: &mut Response<JsonBody>, claims: u64) {
    /// Converts static header values without parsing or allocating.
    const fn hv(v: &'static str) -> HeaderValue {
        HeaderValue::from_static(v)
    }

    let headers = response.headers_mut();
    if let Ok(val) = HeaderValue::from_str(&claims.to_string()) {
        headers.insert(REMOTE_ACCOUNT_CLAIMS_HEADER, val);
    }

    headers.insert(CONTENT_TYPE, hv("application/json"));
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, hv("*"));
    headers.insert(ACCESS_CONTROL_ALLOW_METHODS, hv("POST, OPTIONS, GET"));
    headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, hv("*"));
    headers.insert(ACCESS_CONTROL_MAX_AGE, hv("86400"));
}
