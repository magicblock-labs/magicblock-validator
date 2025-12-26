use std::ops::Deref;

use magicblock_config::types::network::Remote;
use url::Url;

use super::errors::RemoteAccountProviderError;

#[derive(Debug, Clone)]
pub enum Endpoint {
    Rpc { url: String },
    WebSocket { url: String },
    Grpc { url: String, api_key: String },
}

impl Endpoints {
    /// Returns the URL of the first RPC endpoint found in the provided
    /// slice. If no RPC endpoint is found, returns None.
    pub fn rpc_url(&self) -> Option<String> {
        self.iter().find_map(|ep| {
            if let Endpoint::Rpc { url } = ep {
                Some(url.clone())
            } else {
                None
            }
        })
    }

    pub fn pubsubs(&self) -> Vec<&Endpoint> {
        self.iter()
            .filter(|ep| {
                matches!(ep, Endpoint::WebSocket { .. } | Endpoint::Grpc { .. })
            })
            .collect()
    }
}

impl TryFrom<&[Remote]> for Endpoints {
    type Error = RemoteAccountProviderError;

    fn try_from(configs: &[Remote]) -> Result<Self, Self::Error> {
        let mut endpoints = Vec::with_capacity(configs.len());
        for config in configs {
            let endpoint = Endpoint::try_from(config)?;
            endpoints.push(endpoint);
        }
        Ok(Endpoints(endpoints))
    }
}

impl TryFrom<&Remote> for Endpoint {
    type Error = RemoteAccountProviderError;

    fn try_from(config: &Remote) -> Result<Self, Self::Error> {
        match config {
            Remote::Http(url) => Ok(Endpoint::Rpc {
                url: url.to_string(),
            }),
            Remote::Websocket(url) => Ok(Endpoint::WebSocket {
                url: url.to_string(),
            }),
            Remote::Grpc(url) => {
                let (url, api_key) = parse_url_api_key(url);
                let api_key = api_key.ok_or_else(|| {
                    RemoteAccountProviderError::MissingApiKey(format!(
                        "gRPC endpoint requires api_key: {}",
                        url
                    ))
                })?;
                Ok(Endpoint::Grpc { url, api_key })
            }
        }
    }
}

/// Wrapper around a vector of Endpoints with at least one RPC and one
/// websocket/grpc endpoint guaranteed.
/// Previously this was enforced at construction time but now we rely on
/// the config validation to ensure this.
/// However in order to allow for future extensions we keep this type.
#[derive(Debug)]
pub struct Endpoints(Vec<Endpoint>);

impl Deref for Endpoints {
    type Target = Vec<Endpoint>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> IntoIterator for &'a Endpoints {
    type Item = &'a Endpoint;
    type IntoIter = std::slice::Iter<'a, Endpoint>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl From<&[Endpoint]> for Endpoints {
    fn from(endpoints: &[Endpoint]) -> Self {
        // NOTE: here we assume that at least an RPC and a websocket endpoint
        // were provided which is verified on the config level.
        // See magicblock-config/src/lib.rs ensure_http and ensure_websocket
        Endpoints(endpoints.to_vec())
    }
}

fn parse_url_api_key(url: &Url) -> (String, Option<String>) {
    // Try to extract api-key from query parameters
    if let Some(api_key) = url.query_pairs().find_map(|(k, v)| {
        if k == "api-key" {
            Some(v.to_string())
        } else {
            None
        }
    }) {
        // Build URL without query parameters
        let mut url_without_query = url.clone();
        url_without_query.set_query(None);
        return (url_without_query.to_string(), Some(api_key));
    }

    // Check for api-key in path (last segment after final '/')
    let path_segments: Vec<&str> =
        url.path_segments().map(|s| s.collect()).unwrap_or_default();

    if let Some(last_segment) = path_segments.last() {
        // If the last segment looks like an API key (not empty and contains
        // hex chars or other typical api key patterns), treat it as api-key
        if !last_segment.is_empty()
            && last_segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            let mut base_url = url.clone();
            // Remove the last path segment by setting path without it
            if let Ok(mut segments) = base_url.path_segments_mut() {
                segments.pop();
            }
            return (base_url.to_string(), Some(last_segment.to_string()));
        }
    }

    (url.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_url_without_api_key() {
        let input = Url::parse("https://api.devnet.solana.com").unwrap();
        let (url, api_key) = parse_url_api_key(&input);
        assert_eq!(url, "https://api.devnet.solana.com/");
        assert_eq!(api_key, None);
    }

    #[test]
    fn test_parse_url_with_api_key_via_query() {
        let input =
            Url::parse("https://api.devnet.solana.com?api-key=secret123")
                .unwrap();
        let (url, api_key) = parse_url_api_key(&input);
        assert_eq!(url, "https://api.devnet.solana.com/");
        assert_eq!(api_key, Some("secret123".to_string()));
    }

    #[test]
    fn test_parse_url_with_api_key_in_path() {
        let input = Url::parse(
            "https://magicblo-devd137-9da1.devnet.rpcpool.com/secret-123",
        )
        .unwrap();
        let (url, api_key) = parse_url_api_key(&input);
        assert_eq!(url, "https://magicblo-devd137-9da1.devnet.rpcpool.com/");
        assert_eq!(api_key, Some("secret-123".to_string()));
    }

    #[test]
    fn test_parse_url_with_non_api_key_in_path() {
        let input = Url::parse(
            "https://magicblo-devd137-9da1.devnet.rpcpool.com/notanapi%key",
        )
        .unwrap();
        let (url, api_key) = parse_url_api_key(&input);
        assert_eq!(
            url,
            "https://magicblo-devd137-9da1.devnet.rpcpool.com/notanapi%key"
        );
        assert_eq!(api_key, None);
    }
}
