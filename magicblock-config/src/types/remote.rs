use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

use crate::consts;

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
/// Aliases in the URL field are automatically resolved during deserialization.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteConfig {
    /// The kind of remote connection (rpc, websocket, grpc).
    pub kind: RemoteKind,

    /// The resolved URL for this remote connection.
    /// If an alias was used in the config, it is automatically expanded during deserialization.
    pub url: String,

    /// Optional API key for authentication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl<'de> Deserialize<'de> for RemoteConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case")]
        struct RemoteConfigRaw {
            kind: RemoteKind,
            url: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            api_key: Option<String>,
        }

        let raw = RemoteConfigRaw::deserialize(deserializer)?;

        // Resolve the URL alias based on the kind
        let resolved_url = match raw.kind {
            RemoteKind::Rpc => match raw.url.as_str() {
                "mainnet" => consts::RPC_MAINNET.to_string(),
                "devnet" => consts::RPC_DEVNET.to_string(),
                "local" => consts::RPC_LOCAL.to_string(),
                _ => raw.url,
            },
            RemoteKind::Websocket => match raw.url.as_str() {
                "mainnet" => consts::WS_MAINNET.to_string(),
                "devnet" => consts::WS_DEVNET.to_string(),
                "local" => consts::WS_LOCAL.to_string(),
                _ => raw.url,
            },
            RemoteKind::Grpc => raw.url,
        };

        Ok(RemoteConfig {
            kind: raw.kind,
            url: resolved_url,
            api_key: raw.api_key,
        })
    }
}

impl RemoteConfig {
    /// Returns the resolved URL string, expanding aliases if necessary.
    /// This method handles alias resolution for manually constructed
    /// RemoteConfig instances (in tests or when not going through
    /// deserialization).
    pub fn resolved_url(&self) -> &str {
        match self.kind {
            RemoteKind::Rpc => match self.url.as_str() {
                "mainnet" => consts::RPC_MAINNET,
                "devnet" => consts::RPC_DEVNET,
                "local" => consts::RPC_LOCAL,
                _ => &self.url,
            },
            RemoteKind::Websocket => match self.url.as_str() {
                "mainnet" => consts::WS_MAINNET,
                "devnet" => consts::WS_DEVNET,
                "local" => consts::WS_LOCAL,
                _ => &self.url,
            },
            RemoteKind::Grpc => &self.url,
        }
    }

    /// Parses the resolved URL and returns a valid `Url` object.
    pub fn parse_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(self.resolved_url())
    }
}
