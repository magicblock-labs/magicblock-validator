// NOTE: pieces extracted from rpc-client-api/src/config.rs
use base64::{prelude::BASE64_STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Read, str::FromStr};

use solana_sdk::{
    account::WritableAccount,
    clock::{Epoch, Slot},
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcAccountInfoConfig {
    pub encoding: Option<UiAccountEncoding>,
    pub data_slice: Option<UiDataSliceConfig>,
    #[serde(flatten)]
    pub commitment: Option<CommitmentConfig>,
    pub min_context_slot: Option<Slot>,
}

// -----------------
// Account Decoder structs
// -----------------

// NOTE: the below structs pulled from account-decoder/src/lib.rs to avoid
// pulling in the entire crate which depends on token programs and thus causes
// incompat issues + we most likely don't want to support token UiAmounts and similar
// right inside our validator

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum UiAccountEncoding {
    Binary, // Legacy. Retained for RPC backwards compatibility
    Base58,
    Base64,
    JsonParsed,
    #[serde(rename = "base64+zstd")]
    Base64Zstd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiDataSliceConfig {
    pub offset: usize,
    pub length: usize,
}

/// A duplicate representation of an Account for pretty JSON serialization
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UiAccount {
    pub lamports: u64,
    pub data: UiAccountData,
    pub owner: String,
    pub executable: bool,
    pub rent_epoch: Epoch,
    pub space: Option<u64>,
}

impl UiAccount {
    pub fn decode<T: WritableAccount>(&self) -> Option<T> {
        let data = self.data.decode()?;
        Some(T::create(
            self.lamports,
            data,
            Pubkey::from_str(&self.owner).ok()?,
            self.executable,
            self.rent_epoch,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", untagged)]
pub enum UiAccountData {
    LegacyBinary(String), // Legacy. Retained for RPC backwards compatibility
    Json(ParsedAccount),
    Binary(String, UiAccountEncoding),
}

// NOTE: this one from account-decoder/src/parse_account_data.rs
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ParsedAccount {
    pub program: String,
    pub parsed: Value,
    pub space: u64,
}

impl UiAccountData {
    /// Returns decoded account data in binary format if possible
    pub fn decode(&self) -> Option<Vec<u8>> {
        match self {
            UiAccountData::Json(_) => None,
            UiAccountData::LegacyBinary(blob) => bs58::decode(blob).into_vec().ok(),
            UiAccountData::Binary(blob, encoding) => match encoding {
                UiAccountEncoding::Base58 => bs58::decode(blob).into_vec().ok(),
                UiAccountEncoding::Base64 => BASE64_STANDARD.decode(blob).ok(),
                UiAccountEncoding::Base64Zstd => {
                    BASE64_STANDARD.decode(blob).ok().and_then(|zstd_data| {
                        let mut data = vec![];
                        zstd::stream::read::Decoder::new(zstd_data.as_slice())
                            .and_then(|mut reader| reader.read_to_end(&mut data))
                            .map(|_| data)
                            .ok()
                    })
                }
                UiAccountEncoding::Binary | UiAccountEncoding::JsonParsed => None,
            },
        }
    }
}
