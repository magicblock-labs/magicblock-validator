// NOTE: from version/src/lib.rs with MagicBlock Validator added
#![cfg_attr(RUSTC_WITH_SPECIALIZATION, feature(min_specialization))]

use std::{convert::TryInto, fmt};

use serde::{Deserialize, Serialize};
use solana_sanitize::Sanitize;
#[macro_use]
extern crate solana_frozen_abi_macro;

#[derive(Debug, Eq, PartialEq)]
enum ClientId {
    SolanaLabs,
    JitoLabs,
    Firedancer,
    MagicBlock,
    // If new variants are added, update From<u16> and TryFrom<ClientId>.
    Unknown(u16),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, AbiExample)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub commit: u32,      // first 4 bytes of the sha1 commit hash
    pub feature_set: u32, // first 4 bytes of the FeatureSet identifier
    client: u16,
    pub solana_core: String,
    pub git_version: String,
}

impl Version {
    pub fn as_semver_version(&self) -> semver::Version {
        semver::Version::new(
            self.major as u64,
            self.minor as u64,
            self.patch as u64,
        )
    }

    fn client(&self) -> ClientId {
        ClientId::from(self.client)
    }
}

fn compute_commit(sha1: Option<&'static str>) -> Option<u32> {
    u32::from_str_radix(sha1?.get(..8)?, /*radix:*/ 16).ok()
}

impl Default for Version {
    fn default() -> Self {
        let feature_set = u32::from_le_bytes(
            solana_feature_set::ID.as_ref()[..4].try_into().unwrap(),
        );
        Self {
            major: env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap(),
            minor: env!("CARGO_PKG_VERSION_MINOR").parse().unwrap(),
            patch: env!("CARGO_PKG_VERSION_PATCH").parse().unwrap(),
            commit: compute_commit(option_env!("CI_COMMIT"))
                .unwrap_or_default(),
            feature_set,
            // Other client implementations need to modify this line.
            client: u16::try_from(ClientId::MagicBlock).unwrap(),
            solana_core:
                solana_rpc_client_api::response::RpcApiVersion::default()
                    .to_string(),
            git_version: git_version::git_version!().to_string(),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch,)
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{} (src:{:08x}; feat:{}, client:{:?})",
            self.major,
            self.minor,
            self.patch,
            self.commit,
            self.feature_set,
            self.client(),
        )
    }
}

impl Sanitize for Version {}

impl From<u16> for ClientId {
    fn from(client: u16) -> Self {
        match client {
            0u16 => Self::SolanaLabs,
            1u16 => Self::JitoLabs,
            2u16 => Self::Firedancer,
            3u16 => Self::MagicBlock,
            _ => Self::Unknown(client),
        }
    }
}

impl TryFrom<ClientId> for u16 {
    type Error = String;

    fn try_from(client: ClientId) -> Result<Self, Self::Error> {
        match client {
            ClientId::SolanaLabs => Ok(0u16),
            ClientId::JitoLabs => Ok(1u16),
            ClientId::Firedancer => Ok(2u16),
            ClientId::MagicBlock => Ok(3u16),
            ClientId::Unknown(client @ 0u16..=3u16) => {
                Err(format!("Invalid client: {client}"))
            }
            ClientId::Unknown(client) => Ok(client),
        }
    }
}

#[macro_export]
macro_rules! semver {
    () => {
        &*format!("{}", $crate::Version::default())
    };
}

#[macro_export]
macro_rules! version {
    () => {
        &*format!("{:?}", $crate::Version::default())
    };
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_compute_commit() {
        assert_eq!(compute_commit(None), None);
        assert_eq!(compute_commit(Some("1234567890")), Some(0x1234_5678));
        assert_eq!(compute_commit(Some("HEAD")), None);
        assert_eq!(compute_commit(Some("garbagein")), None);
    }

    #[test]
    fn test_client_id() {
        assert_eq!(ClientId::from(0u16), ClientId::SolanaLabs);
        assert_eq!(ClientId::from(1u16), ClientId::JitoLabs);
        assert_eq!(ClientId::from(2u16), ClientId::Firedancer);
        assert_eq!(ClientId::from(3u16), ClientId::MagicBlock);
        for client in 4u16..=u16::MAX {
            assert_eq!(ClientId::from(client), ClientId::Unknown(client));
        }
        assert_eq!(u16::try_from(ClientId::SolanaLabs), Ok(0u16));
        assert_eq!(u16::try_from(ClientId::JitoLabs), Ok(1u16));
        assert_eq!(u16::try_from(ClientId::Firedancer), Ok(2u16));
        assert_eq!(u16::try_from(ClientId::MagicBlock), Ok(3u16));
        for client in 0..=3u16 {
            assert_eq!(
                u16::try_from(ClientId::Unknown(client)),
                Err(format!("Invalid client: {client}"))
            );
        }
        for client in 4u16..=u16::MAX {
            assert_eq!(u16::try_from(ClientId::Unknown(client)), Ok(client));
        }
    }
}
