use crate::consts;
use derive_more::{Display, FromStr};
use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use std::convert::Infallible;
use std::fmt::{Debug, Display};
use std::net::SocketAddr;
use std::str::FromStr;

/// A network bind address that can be parsed from a string like "0.0.0.0:8080".
#[derive(Clone, Debug, Deserialize, Serialize, FromStr, Display)]
#[serde(transparent)]
pub struct BindAddress(pub SocketAddr);

impl Default for BindAddress {
    fn default() -> Self {
        consts::DEFAULT_RPC_ADDR.parse().unwrap()
    }
}

/// A wrapper for `solana_pubkey::Pubkey` to enable deserializing from Base58.
#[derive(
    Clone, Debug, DeserializeFromStr, SerializeDisplay, FromStr, Display,
)]
pub struct SerdePubkey(pub Pubkey);

/// A wrapper for `solana_keypair::Keypair` to enable Serde.
#[derive(DeserializeFromStr, SerializeDisplay, PartialEq)]
pub struct SerdeKeypair(pub Keypair);

impl Clone for SerdeKeypair {
    fn clone(&self) -> Self {
        Self(self.0.insecure_clone())
    }
}

impl FromStr for SerdeKeypair {
    type Err = Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Keypair::from_base58_string(s)))
    }
}

impl Display for SerdeKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_base58_string())
    }
}

impl Debug for SerdeKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}
