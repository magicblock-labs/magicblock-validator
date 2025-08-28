use std::fmt;

use json::{Deserialize, Serialize};
use magicblock_core::link::blocks::BlockHash;
use serde::{
    de::{self, Visitor},
    ser::Error as _,
    Deserializer, Serializer,
};
use solana_pubkey::Pubkey;
use solana_signature::{Signature, SIGNATURE_BYTES};

/// A newtype wrapper for `solana_signature::Signature` to provide a custom
/// `serde` implementation for Base58 encoding.
#[derive(Clone)]
pub struct SerdeSignature(pub Signature);

/// A newtype wrapper for a generic 32-byte array to provide a custom `serde`
/// implementation for Base58 encoding.
///
/// This is used as a common serializer/deserializer for 32-byte types like
/// `Pubkey` and `BlockHash`.
#[derive(Clone)]
pub struct Serde32Bytes(pub [u8; 32]);

impl From<Serde32Bytes> for Pubkey {
    fn from(value: Serde32Bytes) -> Self {
        Self::from(value.0)
    }
}

impl From<Serde32Bytes> for BlockHash {
    fn from(value: Serde32Bytes) -> Self {
        Self::from(value.0)
    }
}

impl From<Pubkey> for Serde32Bytes {
    fn from(value: Pubkey) -> Self {
        Self(value.to_bytes())
    }
}

impl From<BlockHash> for Serde32Bytes {
    fn from(value: BlockHash) -> Self {
        Self(value.to_bytes())
    }
}

impl Serialize for Serde32Bytes {
    /// Serializes the 32-byte array into a Base58 encoded string.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 32 bytes will expand to at most 44 base58 characters
        let mut buf = [0u8; 44];
        let size = bs58::encode(&self.0)
            .onto(buf.as_mut_slice())
            .map_err(S::Error::custom)?;
        // SAFETY:
        // The `bs58` crate guarantees that its encoded output is valid UTF-8.
        serializer.serialize_str(unsafe {
            std::str::from_utf8_unchecked(&buf[..size])
        })
    }
}

impl<'de> Deserialize<'de> for Serde32Bytes {
    /// Deserializes a Base58 encoded string into a 32-byte array.
    /// It returns an error if the decoded data is not exactly 32 bytes.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Serde32BytesVisitor;

        impl Visitor<'_> for Serde32BytesVisitor {
            type Value = Serde32Bytes;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter
                    .write_str("a Base58 string representing a 32-byte array")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let mut buffer = [0u8; 32];
                let decoded_len = bs58::decode(value)
                    .onto(&mut buffer)
                    .map_err(de::Error::custom)?;
                if decoded_len != 32 {
                    return Err(de::Error::custom(format!(
                        "expected 32 bytes, got {}",
                        decoded_len
                    )));
                }
                Ok(Serde32Bytes(buffer))
            }
        }
        deserializer.deserialize_str(Serde32BytesVisitor)
    }
}

impl Serialize for SerdeSignature {
    /// Serializes the 64-byte signature into a Base58 encoded string.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 64 bytes will expand to at most 88 base58 characters
        let mut buf = [0u8; 88];
        let size = bs58::encode(&self.0)
            .onto(buf.as_mut_slice())
            .expect("bs58 buffer is correctly sized");
        // SAFETY:
        // The `bs58` crate guarantees that its encoded output is valid UTF-8.
        serializer.serialize_str(unsafe {
            std::str::from_utf8_unchecked(&buf[..size])
        })
    }
}

impl<'de> Deserialize<'de> for SerdeSignature {
    /// Deserializes a Base58 encoded string into a 64-byte `Signature`.
    /// It returns an error if the decoded data is not exactly 64 bytes.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SerdeSignatureVisitor;

        impl Visitor<'_> for SerdeSignatureVisitor {
            type Value = SerdeSignature;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "a Base58 encoded string representing a 64-byte signature",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let mut buffer = [0u8; SIGNATURE_BYTES];
                let decoded_len = bs58::decode(value)
                    .onto(&mut buffer)
                    .map_err(de::Error::custom)?;
                if decoded_len != SIGNATURE_BYTES {
                    return Err(de::Error::custom(format!(
                        "expected {} bytes, got {}",
                        SIGNATURE_BYTES, decoded_len
                    )));
                }
                Ok(SerdeSignature(Signature::from(buffer)))
            }
        }
        deserializer.deserialize_str(SerdeSignatureVisitor)
    }
}
