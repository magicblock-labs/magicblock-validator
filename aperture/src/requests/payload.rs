use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use hyper::body::{Body, Bytes, Frame, SizeHint};
use json::{Serialize, Value};
use magicblock_core::Slot;

use crate::{RpcResult, error::RpcError, state::subscriptions::SubscriptionID};

/// Serialized JSON bytes shared by RPC envelope encoders and HTTP transport.
pub(crate) struct JsonBody(pub(crate) Vec<u8>);

impl JsonBody {
    pub(crate) fn encode(value: impl Serialize) -> RpcResult<Self> {
        json::to_vec(&value)
            .map(Self)
            .map_err(|error| RpcError::internal(format!("response serialization failed: {error}")))
    }
}

impl Body for JsonBody {
    type Data = Bytes;
    type Error = Infallible;

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.0.len() as u64)
    }

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.0.is_empty() {
            Poll::Ready(None)
        } else {
            Poll::Ready(Some(Ok(Frame::data(std::mem::take(&mut self.0).into()))))
        }
    }
}

/// Represents a JSON-RPC 2.0 Notification object, used for pub/sub updates.
/// It is generic over the type of the result payload.
#[derive(Serialize)]
pub(crate) struct NotificationPayload<T> {
    jsonrpc: &'static str,
    method: &'static str,
    params: NotificationParams<T>,
}

/// Represents a successful JSON-RPC 2.0 Response object.
/// It is generic over the type of the result payload.
#[derive(Serialize)]
pub(crate) struct ResponsePayload<'id, R> {
    jsonrpc: &'static str,
    result: R,
    id: &'id Value,
}

/// Represents a JSON-RPC 2.0 Error Response object.
#[derive(Serialize)]
pub(crate) struct ResponseErrorPayload<'id> {
    jsonrpc: &'static str,
    error: RpcError,
    /// The request ID, which is optional in case of parse errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'id Value>,
}

/// The `params` field of a pub/sub notification, containing the result and subscription ID.
#[derive(Serialize)]
struct NotificationParams<R> {
    result: R,
    subscription: SubscriptionID,
}

/// A standard wrapper that pairs a response `value` with a `context` object,
/// as is common in the Solana RPC API.
#[derive(Serialize)]
pub(crate) struct PayloadResult<T> {
    context: PayloadContext,
    value: T,
}

/// The `context` object for a response, containing the `slot` at which the data is relevant.
#[derive(Serialize)]
struct PayloadContext {
    slot: u64,
}

impl<T: Serialize> NotificationPayload<PayloadResult<T>> {
    /// Serializes a notification that includes a standard `context` object (with a `slot`).
    /// Returns the raw `Bytes` suitable for sending over a WebSocket.
    pub(crate) fn encode(
        value: T,
        slot: u64,
        method: &'static str,
        subscription: SubscriptionID,
    ) -> RpcResult<Bytes> {
        let context = PayloadContext { slot };
        let result = PayloadResult { value, context };
        let params = NotificationParams { result, subscription };
        let notification = Self { jsonrpc: "2.0", method, params };
        JsonBody::encode(notification).map(|body| Bytes::from(body.0))
    }
}

impl<T: Serialize> NotificationPayload<T> {
    /// Serializes a notification for results that do not require a `context` object.
    /// Returns the raw `Bytes` suitable for sending over a WebSocket.
    pub(crate) fn encode_no_context(
        result: T,
        method: &'static str,
        subscription: SubscriptionID,
    ) -> RpcResult<Bytes> {
        let params = NotificationParams { result, subscription };
        let notification = Self { jsonrpc: "2.0", method, params };
        JsonBody::encode(notification).map(|body| Bytes::from(body.0))
    }
}

impl<'id> ResponseErrorPayload<'id> {
    pub(crate) fn encode(id: Option<&'id Value>, error: RpcError) -> RpcResult<JsonBody> {
        JsonBody::encode(Self { jsonrpc: "2.0", error, id })
    }
}

impl<'id, T: Serialize> ResponsePayload<'id, PayloadResult<T>> {
    pub(crate) fn encode(id: &'id Value, value: T, slot: Slot) -> RpcResult<JsonBody> {
        let result = PayloadResult {
            value,
            context: PayloadContext { slot },
        };
        JsonBody::encode(Self { jsonrpc: "2.0", id, result })
    }
}

impl<'id, T: Serialize> ResponsePayload<'id, T> {
    pub(crate) fn encode_no_context(id: &'id Value, result: T) -> RpcResult<JsonBody> {
        JsonBody::encode(Self { jsonrpc: "2.0", id, result })
    }
}
