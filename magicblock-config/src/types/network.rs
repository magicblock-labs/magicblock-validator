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

impl BindAddress {
    fn as_connect_addr(&self) -> SocketAddr {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

        match self.0.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.0.port())
            }
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), self.0.port())
            }
            _ => self.0,
        }
    }

    pub fn http(&self) -> String {
        format!("http://{}", self.as_connect_addr())
    }

    pub fn websocket(&self) -> String {
        format!("ws://{}", self.as_connect_addr())
    }
}

/// A connection to one or more remote clusters (e.g., "devnet").
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case", untagged)]
pub enum RemoteCluster {
    Single(Remote),
    Multiple(Vec<Remote>),
}

impl RemoteCluster {
    pub fn http(&self) -> &Url {
        let remote = match self {
            Self::Single(r) => r,
            Self::Multiple(rs) => {
                rs.first().expect("non-empty remote cluster array")
            }
        };
        match remote {
            Remote::Unified(url) => &url.0,
            Remote::Disjointed { http, .. } => &http.0,
        }
    }

    pub fn websocket(&self) -> Box<dyn Iterator<Item = Url> + '_> {
        fn ws(remote: &Remote) -> Url {
            match remote {
                Remote::Unified(url) => {
                    let mut url = Url::clone(&url.0);
                    let scheme =
                        if url.scheme() == "https" { "wss" } else { "ws" };
                    let _ = url.set_scheme(scheme);
                    if let Some(port) = url.port() {
                        // By solana convention, websocket listens on rpc port + 1
                        let _ = url.set_port(Some(port + 1));
                    }
                    url
                }
                Remote::Disjointed { ws, .. } => ws.0.clone(),
            }
        }
        match self {
            Self::Single(r) => Box::new(iter::once(ws(r))),
            Self::Multiple(rs) => Box::new(rs.iter().map(ws)),
        }
    }
}

impl FromStr for RemoteCluster {
    type Err = url::ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AliasedUrl::from_str(s).map(|url| Self::Single(Remote::Unified(url)))
    }
}

impl Default for RemoteCluster {
    fn default() -> Self {
        consts::DEFAULT_REMOTE
            .parse()
            .expect("Default remote should be valid")
    }
}

/// A connection to a single remote node.
#[serde_as]
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case", untagged)]
pub enum Remote {
    Http(AliasedUrl),
    Websocket(AliasedUrl),
    Grpc(AliasedUrl),
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

        let parsed = AliasedUrl::from_str(&s)?;
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
        Some(Self::Websocket(AliasedUrl(url)))
    }
}

/// A URL that can be aliased with shortcuts like "mainnet".
///
/// Aliases are resolved during parsing and replaced with their full URLs.
#[derive(
    Clone, Debug, Deserialize, SerializeDisplay, Display, PartialEq, Deref,
)]
pub struct AliasedUrl(pub Url);

impl AliasedUrl {
    /// Returns the URL as a string reference.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl FromStr for AliasedUrl {
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
