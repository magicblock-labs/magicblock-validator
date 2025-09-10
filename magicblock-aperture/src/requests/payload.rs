use crate::{
    error::RpcError, state::subscriptions::SubscriptionID, utils::JsonBody,
};
use hyper::{body::Bytes, Response};
use json::{Serialize, Value};
use magicblock_core::Slot;

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
    ) -> Option<Bytes> {
        let context = PayloadContext { slot };
        let result = PayloadResult { value, context };
        let params = NotificationParams {
            result,
            subscription,
        };
        let notification = Self {
            jsonrpc: "2.0",
            method,
            params,
        };
        json::to_vec(&notification).ok().map(Bytes::from)
    }
}

impl<T: Serialize> NotificationPayload<T> {
    /// Serializes a notification for results that do not require a `context` object.
    /// Returns the raw `Bytes` suitable for sending over a WebSocket.
    pub(crate) fn encode_no_context(
        result: T,
        method: &'static str,
        subscription: SubscriptionID,
    ) -> Option<Bytes> {
        let params = NotificationParams {
            result,
            subscription,
        };
        let notification = Self {
            jsonrpc: "2.0",
            method,
            params,
        };
        json::to_vec(&notification).ok().map(Bytes::from)
    }
}

impl<'id> ResponseErrorPayload<'id> {
    /// Constructs a full HTTP response for a JSON-RPC error.
    pub(crate) fn encode(
        id: Option<&'id Value>,
        error: RpcError,
    ) -> Response<JsonBody> {
        let this = Self {
            jsonrpc: "2.0",
            error,
            id,
        };
        Response::new(JsonBody::from(this))
    }
}

impl<'id, T: Serialize> ResponsePayload<'id, PayloadResult<T>> {
    /// Constructs a full HTTP response for a successful result that includes a `context` object.
    pub(crate) fn encode(
        id: &'id Value,
        value: T,
        slot: Slot,
    ) -> Response<JsonBody> {
        let context = PayloadContext { slot };
        let result = PayloadResult { value, context };
        let this = Self {
            jsonrpc: "2.0",
            id,
            result,
        };
        Response::new(JsonBody::from(this))
    }
}

impl<'id, T: Serialize> ResponsePayload<'id, T> {
    /// Constructs a full HTTP response for a successful result that does not require a `context` object.
    pub(crate) fn encode_no_context(
        id: &'id Value,
        result: T,
    ) -> Response<JsonBody> {
        Response::new(Self::encode_no_context_raw(id, result))
    }

    /// Serializes a payload into a `JsonBody` without wrapping it in an `HTTP Response`.
    pub(crate) fn encode_no_context_raw(id: &'id Value, result: T) -> JsonBody {
        let this = Self {
            jsonrpc: "2.0",
            id,
            result,
        };
        JsonBody::from(this)
    }
}
