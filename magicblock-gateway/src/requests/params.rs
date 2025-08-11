use std::fmt;

use json::{Deserialize, Serialize};
use magicblock_core::link::blocks::BlockHash;
use serde::{
    de::{self, Visitor},
    Deserializer, Serializer,
};
use solana_pubkey::Pubkey;
use solana_signature::{Signature, SIGNATURE_BYTES};

#[derive(Clone)]
pub struct SerdeSignature(pub Signature);

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
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut buf = [0u8; 44]; // 32 bytes will expand to at most 44 base58 characters
        let size = bs58::encode(&self.0)
            .onto(buf.as_mut_slice())
            .expect("Buffer too small");
        // SAFETY:
        // bs58 always produces valid UTF-8
        serializer.serialize_str(unsafe {
            std::str::from_utf8_unchecked(&buf[..size])
        })
    }
}

impl<'de> Deserialize<'de> for Serde32Bytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Serde32BytesVisitor;

        impl Visitor<'_> for Serde32BytesVisitor {
            type Value = Serde32Bytes;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "a base58 encoded string representing a 32-byte array",
                )
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
                    return Err(de::Error::custom("expected 32 bytes"));
                }
                Ok(Serde32Bytes(buffer))
            }
        }
        deserializer.deserialize_str(Serde32BytesVisitor)
    }
}

impl Serialize for SerdeSignature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut buf = [0u8; 88]; // 64 bytes will expand to at most 88 base58 characters
        let size = bs58::encode(&self.0)
            .onto(buf.as_mut_slice())
            .expect("Buffer too small");
        // SAFETY:
        // bs58 always produces valid UTF-8
        serializer.serialize_str(unsafe {
            std::str::from_utf8_unchecked(&buf[..size])
        })
    }
}

impl<'de> Deserialize<'de> for SerdeSignature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SerdeSignatureVisitor;

        impl Visitor<'_> for SerdeSignatureVisitor {
            type Value = SerdeSignature;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "a base58 encoded string representing a 64-byte array",
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
                    return Err(de::Error::custom("expected 64 bytes"));
                }
                Ok(SerdeSignature(Signature::from(buffer)))
            }
        }
        deserializer.deserialize_str(SerdeSignatureVisitor)
    }
}
