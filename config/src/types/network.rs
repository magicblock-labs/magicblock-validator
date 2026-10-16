use std::{net::SocketAddr, str::FromStr};

use derive_more::{Deref, Display};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use url::Url;

use crate::consts;

/// A URL parsed from an explicit address or a Solana cluster alias.
/// Serialization always emits the resolved URL.
#[derive(Clone, Debug, PartialEq, Deref, Display, DeserializeFromStr, SerializeDisplay)]
pub struct ResolvedUrl(pub Url);

impl FromStr for ResolvedUrl {
    type Err = url::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let url = match s {
            "mainnet" => "https://api.mainnet-beta.solana.com/",
            "testnet" => "https://api.testnet.solana.com/",
            "devnet" => consts::DEVNET_URL,
            "localhost" | "dev" => "http://localhost:8899/",
            url => url,
        };
        Url::parse(url).map(Self)
    }
}

/// A network bind address that can be parsed from a string like "0.0.0.0:8080".
#[derive(Clone, Copy, Debug, Serialize, Display, Deref)]
#[serde(transparent)]
pub struct BindAddress(pub SocketAddr);

// The default RPC address is a fixed, validated constant.
#[allow(clippy::unwrap_used)]
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
        let mut addr = self.as_connect_addr();
        addr.set_port(addr.port().saturating_add(1));
        format!("ws://{}", addr)
    }
}

impl FromStr for BindAddress {
    type Err = std::net::AddrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accept plain port numbers like "7799" by assuming 127.0.0.1 as the host.
        // Try parsing as u16 first; on failure, fall back to standard socket parsing.
        if let Ok(port) = s.trim().parse::<u16>() {
            return Ok(BindAddress(SocketAddr::from(([127, 0, 0, 1], port))));
        }

        // Fallback to standard socket address parsing (e.g. "0.0.0.0:8899", "[::1]:8899")
        s.parse::<SocketAddr>().map(BindAddress)
    }
}

impl<'de> Deserialize<'de> for BindAddress {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StringOrInt {
            Int(u64),
            String(String),
        }

        match StringOrInt::deserialize(deserializer)? {
            StringOrInt::String(s) => s.parse().map_err(de::Error::custom),
            StringOrInt::Int(port) => {
                let port = u16::try_from(port)
                    .map_err(|_| de::Error::custom("port number out of range for u16"))?;
                Ok(BindAddress(SocketAddr::from(([127, 0, 0, 1], port))))
            }
        }
    }
}

/// Derives the subscription endpoint using Solana's explicit RPC port plus one convention.
/// The input must be an HTTP or HTTPS URL.
pub fn websocket_url(mut url: Url) -> Url {
    let scheme = if url.scheme() == "http" { "ws" } else { "wss" };
    let _ = url.set_scheme(scheme);
    if let Some(port) = url.port() {
        let _ = url.set_port(Some(port + 1));
    }
    url
}
