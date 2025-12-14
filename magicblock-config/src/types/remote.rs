use serde::{
    de::{self, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
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
        struct RemoteConfigVisitor;

        impl<'de> Visitor<'de> for RemoteConfigVisitor {
            type Value = RemoteConfig;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a remote configuration object")
            }

            fn visit_map<A>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut kind: Option<RemoteKind> = None;
                let mut url: Option<String> = None;
                let mut api_key: Option<String> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "kind" => kind = Some(map.next_value()?),
                        "url" => url = Some(map.next_value()?),
                        "api-key" => api_key = map.next_value()?,
                        _ => {
                            // Ignore unknown fields - let the value be consumed
                            let _ = map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }

                let kind = kind.ok_or_else(|| de::Error::missing_field("kind"))?;
                let url = url.ok_or_else(|| de::Error::missing_field("url"))?;

                // Resolve the URL alias based on the kind
                let resolved_url = match kind {
                    RemoteKind::Rpc => match url.as_str() {
                        "mainnet" => consts::RPC_MAINNET.to_string(),
                        "devnet" => consts::RPC_DEVNET.to_string(),
                        "local" => consts::RPC_LOCAL.to_string(),
                        _ => url,
                    },
                    RemoteKind::Websocket => match url.as_str() {
                        "mainnet" => consts::WS_MAINNET.to_string(),
                        "devnet" => consts::WS_DEVNET.to_string(),
                        "local" => consts::WS_LOCAL.to_string(),
                        _ => url,
                    },
                    RemoteKind::Grpc => url,
                };

                Ok(RemoteConfig {
                    kind,
                    url: resolved_url,
                    api_key,
                })
            }
        }

        deserializer.deserialize_map(RemoteConfigVisitor)
    }
}

impl RemoteConfig {
    /// Returns the resolved URL string.
    /// For deserialized configs, the URL is already resolved during deserialization.
    /// For manually constructed instances in tests, this will resolve any known aliases.
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
