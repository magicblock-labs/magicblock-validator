//! Build identifiers shared by RPC version reporting and the startup banner.

use git_version::git_version;
use serde::Serialize;
use solana_feature_set::ID;
use solana_rpc_client_api::response::RpcApiVersion;

/// Build metadata, not a guarantee of deployment health or peer compatibility.
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Version {
    /// MagicBlock workspace package version.
    pub magicblock_core: &'static str,
    /// Solana RPC API compatibility version.
    pub solana_core: String,
    /// First four bytes of the Solana feature-set identifier, in little-endian order.
    pub feature_set: u32,
    /// Git description of the MagicBlock build.
    #[serde(rename = "git-commit")]
    pub git_version: &'static str,
}

impl Default for Version {
    fn default() -> Self {
        let [a, b, c, d, ..] = ID.to_bytes();
        Self {
            magicblock_core: env!("CARGO_PKG_VERSION"),
            solana_core: RpcApiVersion::default().to_string(),
            feature_set: u32::from_le_bytes([a, b, c, d]),
            git_version: git_version!(),
        }
    }
}
