use derive_more::{Display, FromStr};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use std::convert::Infallible;
use std::str::FromStr;

/// A wrapper for `solana_pubkey::Pubkey` to enable deserializing from Base58 strings.
#[derive(
    Clone, Debug, DeserializeFromStr, SerializeDisplay, FromStr, Display,
)]
pub struct SerdePubkey(pub Pubkey);

/// A wrapper for `solana_keypair::Keypair` to enable Serde operations.
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

impl std::fmt::Display for SerdeKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_base58_string())
    }
}

impl std::fmt::Debug for SerdeKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}
