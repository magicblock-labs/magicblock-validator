use std::{fmt::Display, path::PathBuf};

pub mod crypto;
pub mod network;

// Re-export types for easy access
pub use crypto::{SerdeKeypair, SerdePubkey};
use derive_more::{Deref, FromStr};
pub use network::{AliasedUrl, BindAddress, Remote, RemoteCluster};
use serde_with::{DeserializeFromStr, SerializeDisplay};

use crate::consts;

#[derive(
    Clone, Debug, DeserializeFromStr, SerializeDisplay, FromStr, Deref,
)]
pub struct StorageDirectory(pub PathBuf);

impl Display for StorageDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

impl Default for StorageDirectory {
    fn default() -> Self {
        Self(consts::DEFAULT_STORAGE_DIRECTORY.parse().unwrap())
    }
}
