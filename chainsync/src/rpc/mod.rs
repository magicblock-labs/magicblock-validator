//! Shared JSON-RPC envelopes, account encoding policy, and provider diagnostics.

use account::ENCODING;
use derive_more::Display;
use json::Value;
use serde::{Deserialize, Serialize};

pub use account::DecodeError;

/// The provider's JSON-RPC error, including optional diagnostic data.
#[derive(Debug, Deserialize, Display, derive_more::Error)]
#[display("RPC {code}: {message}")]
pub struct Error {
    /// Provider's JSON-RPC error code, retained without reclassification.
    pub code: i64,
    /// Provider's human-readable explanation.
    pub message: String,
    /// Optional provider-specific diagnostics retained with the RPC error.
    pub data: Option<Value>,
}

pub(crate) use account::WireAccount;

/// Typed request wrapped in the common JSON-RPC envelope.
#[derive(Serialize)]
pub(crate) struct Request<P> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: P,
}

impl<P> Request<P> {
    pub(crate) fn new(id: u64, method: &'static str, params: P) -> Self {
        Self {
            jsonrpc: VERSION,
            id,
            method,
            params,
        }
    }
}

/// Account response policy shared by HTTP and WebSocket requests.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountConfig {
    encoding: &'static str,
    commitment: &'static str,
    /// Minimum response context slot for HTTP; omitted from WebSocket subscriptions.
    #[serde(skip_serializing_if = "Option::is_none")]
    min_context_slot: Option<u64>,
}

impl AccountConfig {
    /// Requests confirmed `base64+zstd` accounts; only HTTP sets a slot floor.
    pub(crate) fn new(min_context_slot: Option<u64>) -> Self {
        Self {
            encoding: ENCODING,
            commitment: COMMITMENT,
            min_context_slot,
        }
    }
}

/// Provider context applying to one response value.
#[derive(Deserialize)]
pub(crate) struct Context {
    /// Confirmed slot of the accompanying value.
    pub(crate) slot: u64,
}

/// RPC response context and payload; a missing `value` is an error even when `T` is optional.
#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub(crate) struct ContextValue<T> {
    pub(crate) context: Context,
    /// Explicit null is valid for `Option<T>`, but omitting the field is not.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub(crate) value: T,
}

pub(crate) const VERSION: &str = "2.0";
const COMMITMENT: &str = "confirmed";

/// Compressed account-wire decoding and its typed failures.
mod account;
