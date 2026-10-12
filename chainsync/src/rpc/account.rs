use std::io;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use solana_account::AccountBuilder;
use solana_pubkey::Pubkey;

/// Invalid encoded account data shared by HTTP and WebSocket responses.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("invalid account base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid account owner: {0}")]
    Owner(#[from] solana_pubkey::ParsePubkeyError),
    #[error("invalid account zstd: {0}")]
    Zstd(#[source] io::Error),
    #[error("invalid provider message: {0}")]
    Protocol(&'static str),
}

/// Borrowed RPC account before owner and payload validation.
#[derive(Deserialize)]
pub(crate) struct WireAccount<'a> {
    /// Base64-encoded compressed bytes and their declared encoding.
    #[serde(borrow)]
    data: (&'a str, &'a str),
    /// Base58 owner pending public-key validation.
    owner: &'a str,
    lamports: u64,
    executable: bool,
}

impl WireAccount<'_> {
    /// Validates the declared encoding and owner, then stamps the response slot.
    /// The resulting builder remains `Uninit`; decoding does not establish delegation.
    pub(crate) fn decode(self, slot: u64) -> Result<AccountBuilder, DecodeError> {
        if self.data.1 != ENCODING {
            return Err(DecodeError::Protocol("unsupported account encoding"));
        }
        let owner: Pubkey = self.owner.parse()?;
        let data = STANDARD.decode(self.data.0.as_bytes())?;
        let data = zstd::stream::decode_all(data.as_slice()).map_err(DecodeError::Zstd)?;
        Ok(AccountBuilder::default()
            .owner(owner)
            .lamports(self.lamports)
            .executable(self.executable)
            .slot(slot)
            .data(data))
    }
}

pub(super) const ENCODING: &str = "base64+zstd";
