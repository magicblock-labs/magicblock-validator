use jsonrpc_core::Params;
use jsonrpc_pubsub::{Sink, Subscriber};
use log::*;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PubsubError {
    #[error("Failed to confirm subscription: {0}")]
    FailedToSendSubscription(String),
}

pub type PubsubResult<T> = Result<T, PubsubError>;

// -----------------
// Subscriber Checks
// -----------------
pub fn ensure_params(
    subscriber: Subscriber,
    params: &Params,
) -> Option<Subscriber> {
    if params == &Params::None {
        reject_parse_error(subscriber, "Missing parameters", None::<()>);
        None
    } else {
        Some(subscriber)
    }
}

pub fn ensure_empty_params(
    subscriber: Subscriber,
    params: &Params,
    warn: bool,
) -> Option<Subscriber> {
    if params == &Params::None {
        Some(subscriber)
    } else if warn {
        warn!("Parameters should be empty");
        Some(subscriber)
    } else {
        reject_parse_error(
            subscriber,
            "Parameters should be empty",
            None::<()>,
        );
        None
    }
}

pub fn try_parse_params<D: DeserializeOwned>(
    subscriber: Subscriber,
    params: Params,
) -> Option<(Subscriber, D)> {
    match params.parse() {
        Ok(params) => Some((subscriber, params)),
        Err(err) => {
            reject_parse_error(
                subscriber,
                "Failed to parse parameters",
                Some(err),
            );
            None
        }
    }
}

pub fn ensure_and_try_parse_params<D: DeserializeOwned>(
    subscriber: Subscriber,
    params: Params,
) -> Option<(Subscriber, D)> {
    ensure_params(subscriber, &params)
        .and_then(|subscriber| try_parse_params(subscriber, params))
}

// -----------------
// Subscriber Errors
// -----------------
#[allow(dead_code)]
pub fn reject_internal_error<T: std::fmt::Debug>(
    subscriber: Subscriber,
    msg: &str,
    err: Option<T>,
) {
    _reject_subscriber_error(
        subscriber,
        msg,
        err,
        jsonrpc_core::ErrorCode::InternalError,
    )
}

#[allow(dead_code)]
pub fn reject_parse_error<T: std::fmt::Debug>(
    subscriber: Subscriber,
    msg: &str,
    err: Option<T>,
) {
    _reject_subscriber_error(
        subscriber,
        msg,
        err,
        jsonrpc_core::ErrorCode::ParseError,
    )
}

fn _reject_subscriber_error<T: std::fmt::Debug>(
    subscriber: Subscriber,
    msg: &str,
    err: Option<T>,
    code: jsonrpc_core::ErrorCode,
) {
    let message = match err {
        Some(err) => format!("{msg}: {:?}", err),
        None => msg.to_string(),
    };
    if let Err(reject_err) = subscriber.reject(jsonrpc_core::Error {
        code,
        message,
        data: None,
    }) {
        error!("Failed to reject subscriber: {:?}", reject_err);
    };
}

/// Tries to notify the sink of the error.
/// Returns true if the sink could not be notified
pub fn sink_notify_error(sink: &Sink, msg: String) -> bool {
    error!("{}", msg);
    let map = {
        let mut map = serde_json::Map::new();
        map.insert("error".to_string(), Value::String(msg));
        map
    };

    if let Err(err) = sink.notify(Params::Map(map)) {
        debug!("Subscription has ended, finishing {:?}.", err);
        true
    } else {
        false
    }
}
