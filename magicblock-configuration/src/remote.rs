use crate::consts;
use derive_more::Display;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use std::str::FromStr;
use url::Url;

/// A connection to one or more remote clusters.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case", untagged)]
pub enum RemoteCluster {
    Single(Remote),
    Multiple(Vec<Remote>),
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
