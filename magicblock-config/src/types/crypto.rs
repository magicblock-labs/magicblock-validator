use std::{panic, str::FromStr};

use derive_more::{Deref, Display, FromStr};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

/// A wrapper for `solana_pubkey::Pubkey` to enable deserializing from Base58 strings.
#[derive(
    Clone, Debug, DeserializeFromStr, SerializeDisplay, FromStr, Display,
)]
pub struct SerdePubkey(pub Pubkey);

/// A wrapper for `solana_keypair::Keypair` to enable Serde operations.
#[derive(DeserializeFromStr, SerializeDisplay, PartialEq, Deref)]
pub struct SerdeKeypair(pub(crate) Keypair);

impl Clone for SerdeKeypair {
    fn clone(&self) -> Self {
        Self(self.0.insecure_clone())
    }
}

impl FromStr for SerdeKeypair {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        panic::catch_unwind(|| Keypair::from_base58_string(s))
            .map(Self)
            .map_err(|_| format!("invalid base58 keypair: {}", s))
    }
}

impl std::fmt::Display for SerdeKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_base58_string())
    }
}

impl std::fmt::Debug for SerdeKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.pubkey())
    }
}
