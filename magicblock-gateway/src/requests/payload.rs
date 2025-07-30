use crate::{error::RpcError, state::subscriptions::SubscriptionID, Slot};
use hyper::{body::Bytes, Response};
use json::{Serialize, Value};

use super::http::utils::JsonBody;

#[derive(Serialize)]
pub(crate) struct NotificationPayload<T> {
    jsonrpc: &'static str,
    method: &'static str,
    params: NotificationParams<T>,
}

#[derive(Serialize)]
pub(crate) struct ResponsePayload<'id, R> {
    jsonrpc: &'static str,
    result: R,
    id: &'id Value,
}

#[derive(Serialize)]
pub(crate) struct ResponseErrorPayload<'id> {
    jsonrpc: &'static str,
    error: RpcError,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'id Value>,
}

#[derive(Serialize)]
struct NotificationParams<R> {
    result: R,
    subscription: SubscriptionID,
}

#[derive(Serialize)]
pub(crate) struct PayloadResult<T> {
    context: PayloadContext,
    value: T,
}

#[derive(Serialize)]
struct PayloadContext {
    slot: u64,
}

impl<T: Serialize> NotificationPayload<PayloadResult<T>> {
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
    pub(crate) fn encode_no_context(id: &'id Value, result: T) -> JsonBody {
        let this = Self {
            jsonrpc: "2.0",
            id,
            result,
        };
        JsonBody::from(this)
    }
}
