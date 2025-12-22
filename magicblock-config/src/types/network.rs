use std::{net::SocketAddr, str::FromStr};

use derive_more::{Deref, Display, FromStr};
use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};
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

/// A remote endpoint for syncing with the base chain.
///
/// Supported types:
/// - **Http**: JSON-RPC HTTP endpoint (scheme: `http` or `https`)
/// - **Websocket**: WebSocket endpoint for PubSub subscriptions (scheme: `ws` or `wss`)
/// - **Grpc**: gRPC endpoint for streaming (schemes `grpc`/`grpcs` are converted to `http`/`https`)
#[derive(Clone, DeserializeFromStr, SerializeDisplay, Display, Debug)]
pub enum Remote {
    Http(ResolvedUrl),
    Websocket(ResolvedUrl),
    Grpc(ResolvedUrl),
}

impl FromStr for Remote {
    type Err = url::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Handle non-standard schemes by detecting them before parsing
        let mut s = s.to_owned();
        let is_grpc = s.starts_with("grpc");
        if is_grpc {
            // SAFETY:
            // We made sure that "grpc" is the prefix and we are not violating Unicode invariants
            unsafe { s.as_bytes_mut()[0..4].copy_from_slice(b"http") };
        }

        let parsed = ResolvedUrl::from_str(&s)?;
        let remote = match parsed.0.scheme() {
            _ if is_grpc => Self::Grpc(parsed),
            "http" | "https" => Self::Http(parsed),
            "ws" | "wss" => Self::Websocket(parsed),
            _ => return Err(url::ParseError::InvalidDomainCharacter),
        };
        Ok(remote)
    }
}

impl Remote {
    /// Returns the URL as a string reference.
    pub fn url_str(&self) -> &str {
        match self {
            Self::Http(u) => u.as_str(),
            Self::Websocket(u) => u.as_str(),
            Self::Grpc(u) => u.as_str(),
        }
    }

    /// Converts an HTTP remote to a WebSocket remote by deriving the appropriate WebSocket URL.
    pub(crate) fn to_websocket(&self) -> Option<Self> {
        let mut url = match self {
            Self::Websocket(_) => return Some(self.clone()),
            Self::Grpc(_) => return None,
            Self::Http(u) => u.0.clone(),
        };
        let _ = if url.scheme() == "http" {
            url.set_scheme("ws")
        } else {
            url.set_scheme("wss")
        };
        if let Some(port) = url.port() {
            // As per solana convention websocket port is one greater than http
            let _ = url.set_port(Some(port + 1));
        }
        Some(Self::Websocket(ResolvedUrl(url)))
    }
}

/// A URL that whose alias like "mainnet" was resolved.
///
/// Aliases are resolved during parsing and replaced with their full URLs.
#[derive(
    Clone, Debug, Deserialize, SerializeDisplay, Display, PartialEq, Deref,
)]
pub struct ResolvedUrl(pub Url);

impl ResolvedUrl {
    /// Returns the URL as a string reference.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl FromStr for ResolvedUrl {
    type Err = url::ParseError;

    /// Parses a string into an AliasedUrl, resolving known aliases to their full URLs.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let url_str = match s {
            "mainnet" => consts::MAINNET_URL,
            "devnet" => consts::DEVNET_URL,
            "testnet" => consts::TESTNET_URL,
            "localhost" | "dev" => consts::LOCALHOST_URL,
            custom => custom,
        };
        Url::parse(url_str).map(Self)
    }
}
