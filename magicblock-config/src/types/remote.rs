use std::net::SocketAddr;

use derive_more::{Deref, Display, FromStr};
use serde::{
    de::{self, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use url::Url;

use crate::consts;

/// A network bind address that can be parsed from a string like "0.0.0.0:8080".
#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, FromStr, Display, Deref,
)]
#[serde(transparent)]
pub struct BindAddress(pub SocketAddr);

impl Default for BindAddress {
    fn default() -> Self {
        consts::DEFAULT_RPC_ADDR.parse().unwrap()
    }
}

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

            fn expecting(
                &self,
                formatter: &mut std::fmt::Formatter,
            ) -> std::fmt::Result {
                formatter.write_str("a remote configuration object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
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

                let kind =
                    kind.ok_or_else(|| de::Error::missing_field("kind"))?;
                let url = url.ok_or_else(|| de::Error::missing_field("url"))?;

                // Resolve the URL alias based on the kind
                let resolved_url = resolve_url(kind, &url);

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
    /// Parses the resolved URL and returns a valid `Url` object.
    pub fn parse_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.url)
    }
}

/// Resolves aliases to a URL and passes through custom URLs unchanged.
pub fn resolve_url(kind: RemoteKind, url: &str) -> String {
    match kind {
        RemoteKind::Rpc => match url {
            "mainnet" => consts::RPC_MAINNET.to_string(),
            "devnet" => consts::RPC_DEVNET.to_string(),
            "local" => consts::RPC_LOCAL.to_string(),
            _ => url.to_string(),
        },
        RemoteKind::Websocket => match url {
            "mainnet" => consts::WS_MAINNET.to_string(),
            "devnet" => consts::WS_DEVNET.to_string(),
            "local" => consts::WS_LOCAL.to_string(),
            _ => url.to_string(),
        },
        RemoteKind::Grpc => url.to_string(),
    }
}

/// Parses CLI remote config argument in format: kind:url[?api-key=value]
/// Example: rpc:devnet, websocket:https://api.devnet.solana.com
/// Important: The URL can contain colons (https://), so we match 'kind:'
/// only if it's a valid kind at the start.
pub fn parse_remote_config(s: &str) -> Result<RemoteConfig, String> {
    // Find the kind by looking for a valid kind followed by a colon
    let kinds = ["rpc", "websocket", "grpc"];
    let kind_and_rest = kinds
        .iter()
        .find_map(|k| {
            let prefix = format!("{}:", k);
            if s.starts_with(&prefix) {
                Some((*k, &s[prefix.len()..]))
            } else {
                None
            }
        })
        .ok_or_else(|| {
            "Remote format must start with 'kind:url' where kind is \
             one of: rpc, websocket, grpc. Example: 'rpc:devnet'"
                .to_string()
        })?;

    let kind = match kind_and_rest.0 {
        "rpc" => RemoteKind::Rpc,
        "websocket" => RemoteKind::Websocket,
        "grpc" => RemoteKind::Grpc,
        // SAFETY: we already excluded invalid kinds above
        _ => unreachable!(),
    };

    let rest = kind_and_rest.1;
    let (url, api_key) = if let Some((url, query)) = rest.split_once('?') {
        let api_key = query.strip_prefix("api-key=").map(|s| s.to_string());
        (url.to_string(), api_key)
    } else {
        (rest.to_string(), None)
    };

    // Resolve the URL alias based on the kind
    let resolved_url = resolve_url(kind, &url);

    Ok(RemoteConfig {
        kind,
        url: resolved_url,
        api_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rpc_without_api_key() {
        let result = parse_remote_config("rpc:https://api.devnet.solana.com");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Rpc);
        assert_eq!(config.url, "https://api.devnet.solana.com");
        assert_eq!(config.api_key, None);
    }

    #[test]
    fn test_parse_rpc_with_api_key() {
        let result = parse_remote_config(
            "rpc:https://api.devnet.solana.com?api-key=secret123",
        );
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Rpc);
        assert_eq!(config.url, "https://api.devnet.solana.com");
        assert_eq!(config.api_key, Some("secret123".to_string()));
    }

    #[test]
    fn test_parse_websocket_without_api_key() {
        let result =
            parse_remote_config("websocket:wss://api.devnet.solana.com");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Websocket);
        assert_eq!(config.url, "wss://api.devnet.solana.com");
        assert_eq!(config.api_key, None);
    }

    #[test]
    fn test_parse_websocket_with_api_key() {
        let result = parse_remote_config(
            "websocket:wss://api.devnet.solana.com?api-key=mykey",
        );
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Websocket);
        assert_eq!(config.url, "wss://api.devnet.solana.com");
        assert_eq!(config.api_key, Some("mykey".to_string()));
    }

    #[test]
    fn test_parse_grpc_without_api_key() {
        let result = parse_remote_config("grpc:http://localhost:50051");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Grpc);
        assert_eq!(config.url, "http://localhost:50051");
        assert_eq!(config.api_key, None);
    }

    #[test]
    fn test_parse_grpc_with_api_key() {
        let result =
            parse_remote_config("grpc:http://localhost:50051?api-key=xyz");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Grpc);
        assert_eq!(config.url, "http://localhost:50051");
        assert_eq!(config.api_key, Some("xyz".to_string()));
    }

    #[test]
    fn test_parse_missing_kind() {
        let result = parse_remote_config("https://api.devnet.solana.com");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Remote format must start with"));
    }

    #[test]
    fn test_parse_invalid_kind() {
        let result = parse_remote_config("http:https://api.devnet.solana.com");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Remote format must start with"));
    }

    #[test]
    fn test_parse_empty_url() {
        let result = parse_remote_config("rpc:");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Rpc);
        assert_eq!(config.url, "");
    }

    #[test]
    fn test_parse_api_key_with_special_chars() {
        let result = parse_remote_config(
            "rpc:http://localhost:8899?api-key=abc_123-xyz",
        );
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.api_key, Some("abc_123-xyz".to_string()));
    }

    #[test]
    fn test_parse_url_with_port() {
        let result = parse_remote_config("rpc:http://localhost:8899");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.url, "http://localhost:8899");
    }

    #[test]
    fn test_parse_url_with_path() {
        let result = parse_remote_config("rpc:http://localhost:8899/v1");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.url, "http://localhost:8899/v1");
    }

    // ================================================================
    // Aliases
    // ================================================================

    #[test]
    fn test_parse_rpc_mainnet_alias() {
        let result = parse_remote_config("rpc:mainnet");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Rpc);
        assert_eq!(config.url, consts::RPC_MAINNET);
    }

    #[test]
    fn test_parse_rpc_devnet_alias() {
        let result = parse_remote_config("rpc:devnet");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Rpc);
        assert_eq!(config.url, consts::RPC_DEVNET);
    }

    #[test]
    fn test_parse_rpc_local_alias() {
        let result = parse_remote_config("rpc:local");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Rpc);
        assert_eq!(config.url, consts::RPC_LOCAL);
    }

    #[test]
    fn test_parse_websocket_mainnet_alias() {
        let result = parse_remote_config("websocket:mainnet");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Websocket);
        assert_eq!(config.url, consts::WS_MAINNET);
    }

    #[test]
    fn test_parse_websocket_devnet_alias() {
        let result = parse_remote_config("websocket:devnet");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Websocket);
        assert_eq!(config.url, consts::WS_DEVNET);
    }

    #[test]
    fn test_parse_websocket_local_alias() {
        let result = parse_remote_config("websocket:local");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Websocket);
        assert_eq!(config.url, consts::WS_LOCAL);
    }

    #[test]
    fn test_parse_rpc_alias_with_api_key() {
        let result = parse_remote_config("rpc:mainnet?api-key=secret");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Rpc);
        assert_eq!(config.url, consts::RPC_MAINNET);
        assert_eq!(config.api_key, Some("secret".to_string()));
    }

    #[test]
    fn test_parse_websocket_alias_with_api_key() {
        let result = parse_remote_config("websocket:devnet?api-key=mytoken");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.kind, RemoteKind::Websocket);
        assert_eq!(config.url, consts::WS_DEVNET);
        assert_eq!(config.api_key, Some("mytoken".to_string()));
    }

    #[test]
    fn test_parse_different_kinds_same_alias() {
        // Same alias "mainnet" should resolve to different URLs based on kind
        let rpc = parse_remote_config("rpc:mainnet").unwrap();
        let ws = parse_remote_config("websocket:mainnet").unwrap();

        assert_eq!(rpc.kind, RemoteKind::Rpc);
        assert_eq!(ws.kind, RemoteKind::Websocket);
        assert_eq!(rpc.url, consts::RPC_MAINNET);
        assert_eq!(ws.url, consts::WS_MAINNET);
        assert_ne!(rpc.url, ws.url);
    }
}
