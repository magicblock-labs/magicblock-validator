use std::{fmt::Display, path::PathBuf};

pub mod crypto;
pub mod remote;

// Re-export types for easy access
pub use crypto::{SerdeKeypair, SerdePubkey};
use derive_more::{Deref, FromStr};
pub use remote::{resolve_url, BindAddress, RemoteConfig, RemoteKind};
use serde_with::{DeserializeFromStr, SerializeDisplay};

use crate::consts;

/// A parsed storage directory path for application data.
///
/// Can be deserialized from a string path, which allows flexible configuration
/// via TOML files, environment variables, and CLI arguments.
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
    /// Returns the default storage directory path.
    fn default() -> Self {
        Self(consts::DEFAULT_STORAGE_DIRECTORY.parse().unwrap())
    }
}
