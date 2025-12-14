use serde::{Deserialize, Serialize};
use url::Url;

/// The kind of remote connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteKind {
    /// JSON-RPC HTTP connection.
    Rpc,
    /// WebSocket connection used for subscriptions.
    Websocket,
    /// gRPC connection used for subscriptions.
    Grpc,
}

/// Configuration for a single remote connection.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct RemoteConfig {
    /// The kind of remote connection (rpc, websocket, grpc).
    pub kind: RemoteKind,

    /// The URL for this remote connection.
    pub url: String,

    /// Optional API key for authentication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl RemoteConfig {
    /// Parses the URL field and returns a valid `Url` object.
    pub fn parse_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.url)
    }
}
