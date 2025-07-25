use hyper::body::Bytes;
use json::Serialize;

#[derive(Serialize)]
pub(crate) struct WebsocketNotification<T> {
    jsonrpc: &'static str,
    method: &'static str,
    params: NotificationParams<T>,
}

#[derive(Serialize)]
struct NotificationParams<R> {
    result: R,
    subscription: u64,
}

#[derive(Serialize)]
pub(crate) struct NotificationResult<T> {
    context: NotificationContext,
    value: T,
}

#[derive(Serialize)]
struct NotificationContext {
    slot: u64,
}

impl<T: Serialize> WebsocketNotification<NotificationResult<T>> {
    pub(crate) fn encode(
        value: T,
        slot: u64,
        method: &'static str,
        subscription: u64,
    ) -> Option<Bytes> {
        let context = NotificationContext { slot };
        let result = NotificationResult { value, context };
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

impl<T: Serialize> WebsocketNotification<T> {
    pub(crate) fn encode_no_context(
        result: T,
        method: &'static str,
        subscription: u64,
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
