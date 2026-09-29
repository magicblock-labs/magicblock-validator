use derive_more::{Display, FromStr};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use solana_pubkey::Pubkey;

/// A wrapper for `solana_pubkey::Pubkey` to enable deserializing from Base58 strings.
#[derive(
    Clone, Debug, DeserializeFromStr, SerializeDisplay, FromStr, Display,
)]
pub struct SerdePubkey(pub Pubkey);
