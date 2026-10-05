use std::fmt;

use json::{Deserialize, Serialize};
use serde::{
    Deserializer, Serializer,
    de::{self, Visitor},
    ser::Error as _,
};
use solana_hash::Hash as BlockHash;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

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

impl From<SerdeSignature> for Signature {
    fn from(value: SerdeSignature) -> Self {
        value.0
    }
}

/// Encodes fixed-size public keys and signatures using a stack buffer.
fn serialize_base58<const N: usize, S: Serializer>(
    bytes: &[u8; N],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // The largest supported value is a 64-byte signature (at most 88 characters).
    let mut buffer = [0u8; 88];
    let size = bs58::encode(bytes).onto(buffer.as_mut_slice()).map_err(S::Error::custom)?;
    // SAFETY: bs58 only emits ASCII characters from its alphabet.
    serializer.serialize_str(unsafe { std::str::from_utf8_unchecked(&buffer[..size]) })
}

/// Rejects any Base58 value whose decoded length differs from the target type.
fn deserialize_base58<'de, const N: usize, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<[u8; N], D::Error> {
    /// Decodes directly into the fixed-size destination and rejects short values.
    struct Base58Visitor<const N: usize>;
    impl<const N: usize> Visitor<'_> for Base58Visitor<N> {
        type Value = [u8; N];
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "a Base58 string representing {N} bytes")
        }
        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            let mut buffer = [0u8; N];
            let size = bs58::decode(value).onto(&mut buffer).map_err(E::custom)?;
            if size != N {
                return Err(E::custom(format!("expected {N} bytes, got {size}")));
            }
            Ok(buffer)
        }
    }
    deserializer.deserialize_str(Base58Visitor::<N>)
}

impl Serialize for Serde32Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_base58(&self.0, serializer)
    }
}
impl<'de> Deserialize<'de> for Serde32Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_base58(deserializer).map(Self)
    }
}
impl Serialize for SerdeSignature {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_base58(self.0.as_array(), serializer)
    }
}
impl<'de> Deserialize<'de> for SerdeSignature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_base58(deserializer).map(|bytes: [u8; 64]| Self(Signature::from(bytes)))
    }
}
