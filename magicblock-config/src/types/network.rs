// src/types/network.rs
use std::{iter, net::SocketAddr, str::FromStr};

use derive_more::{Deref, Display, FromStr};
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
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
    /// A single URL for both HTTP and WebSocket connections.
    Unified(#[serde_as(as = "DisplayFromStr")] AliasedUrl),
    /// Separate URLs for HTTP and WebSocket connections.
    Disjointed {
        #[serde_as(as = "DisplayFromStr")]
        http: AliasedUrl,
        #[serde_as(as = "DisplayFromStr")]
        ws: AliasedUrl,
    },
}

/// A URL that can be aliased with shortcuts like "mainnet".
#[derive(Clone, Debug, Deserialize, Serialize, Display, PartialEq)]
pub struct AliasedUrl(pub Url);

impl FromStr for AliasedUrl {
    type Err = url::ParseError;
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
