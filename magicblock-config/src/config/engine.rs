use std::{
    fmt, net::SocketAddr, num::NonZeroU64, path::PathBuf, sync::Arc,
    time::Duration,
};

use nucleus::config::{
    AccountsDBParams, Authority, BlockstoreParams, LedgerParams,
};
use serde::{Deserialize, Serialize};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::{
    consts,
    types::{BindAddress, SerdePubkey},
};

/// Engine configuration parameterized by its replication role.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct EngineConfig<R> {
    /// Local signing identity and represented engine authority.
    pub authority: Authority,
    /// Account storage and recency-cache parameters.
    #[serde(with = "accountsdb::AccountsDBParamsDef")]
    pub accountsdb: AccountsDBParams,
    /// Block timing and superblock parameters.
    #[serde(with = "blockstore::BlockstoreParamsDef")]
    pub blockstore: BlockstoreParams,
    /// Ledger directory and retention parameters.
    #[serde(with = "ledger::LedgerParamsDef")]
    pub ledger: LedgerParams,
    /// Role-specific replication settings.
    pub replication: R,
}

/// Replication settings for a validator that produces blocks.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct LeaderReplication {
    /// TCP address on which followers connect.
    pub bind_address: BindAddress,
    /// Local identities permitted to follow this validator.
    pub allowed_followers: Vec<SerdePubkey>,
}

/// Replication settings for a validator that follows an upstream leader.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct FollowerReplication {
    /// TCP address of the immediate upstream validator.
    pub upstream_address: SocketAddr,
    /// Identity whose signed replication responses are accepted.
    pub upstream_authority: SerdePubkey,
    /// TCP address on which downstream followers connect.
    pub bind_address: BindAddress,
    /// Local identities permitted to follow this verifier.
    pub allowed_followers: Vec<SerdePubkey>,
}

impl<R: Default> Default for EngineConfig<R> {
    fn default() -> Self {
        let local =
            Keypair::try_from_base58_string(consts::DEFAULT_VALIDATOR_KEYPAIR)
                .expect("default validator keypair must be valid");
        Self {
            authority: Arc::new(local).into(),
            accountsdb: default_accountsdb(),
            blockstore: default_blockstore(),
            ledger: default_ledger(),
            replication: R::default(),
        }
    }
}

impl Default for LeaderReplication {
    fn default() -> Self {
        Self {
            bind_address: default_replication_bind_address(),
            allowed_followers: Vec::new(),
        }
    }
}

impl Default for FollowerReplication {
    fn default() -> Self {
        Self {
            upstream_address: SocketAddr::from(([0, 0, 0, 0], 0)),
            upstream_authority: SerdePubkey(Default::default()),
            bind_address: default_replication_bind_address(),
            allowed_followers: Vec::new(),
        }
    }
}

fn default_replication_bind_address() -> BindAddress {
    consts::DEFAULT_REPLICATION_BIND_ADDRESS
        .parse()
        .expect("default replication bind address must be valid")
}

impl<R: fmt::Debug> fmt::Debug for EngineConfig<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineConfig")
            .field("authority", &self.authority.local.pubkey())
            .field("accountsdb_directory", &self.accountsdb.directory)
            .field("accountsdb_lru_capacity", &self.accountsdb.lru_capacity)
            .field("blocktime", &self.blockstore.blocktime)
            .field("superblock", &self.blockstore.superblock)
            .field("ledger_directory", &self.ledger.directory)
            .field("ledger_size_limit", &self.ledger.size_limit)
            .field("replication", &self.replication)
            .finish()
    }
}

fn default_accountsdb() -> AccountsDBParams {
    AccountsDBParams {
        directory: accountsdb::default_directory(),
        lru_capacity: accountsdb::default_lru_capacity(),
    }
}

fn default_blockstore() -> BlockstoreParams {
    BlockstoreParams {
        blocktime: blockstore::default_blocktime(),
        superblock: blockstore::default_superblock(),
    }
}

fn default_ledger() -> LedgerParams {
    LedgerParams {
        directory: ledger::default_directory(),
        size_limit: ledger::default_size_limit(),
    }
}

mod accountsdb {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(
        remote = "AccountsDBParams",
        rename_all = "kebab-case",
        deny_unknown_fields
    )]
    pub(super) struct AccountsDBParamsDef {
        #[serde(default = "default_directory")]
        directory: PathBuf,
        #[serde(default = "default_lru_capacity")]
        lru_capacity: usize,
    }

    pub(super) fn default_directory() -> PathBuf {
        PathBuf::from(consts::DEFAULT_ENGINE_LEDGER_DIRECTORY)
            .join("accountsdb")
    }

    pub(super) const fn default_lru_capacity() -> usize {
        consts::DEFAULT_ACCOUNTS_LRU_CAPACITY
    }
}

mod blockstore {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(
        remote = "BlockstoreParams",
        rename_all = "kebab-case",
        deny_unknown_fields
    )]
    pub(super) struct BlockstoreParamsDef {
        #[serde(default = "default_blocktime", with = "humantime")]
        blocktime: Duration,
        #[serde(default = "default_superblock")]
        superblock: NonZeroU64,
    }

    pub(super) fn default_blocktime() -> Duration {
        Duration::from_millis(consts::DEFAULT_LEDGER_BLOCK_TIME_MS)
    }

    pub(super) fn default_superblock() -> NonZeroU64 {
        NonZeroU64::new(consts::DEFAULT_SUPERBLOCK_SIZE)
            .expect("default superblock size must be non-zero")
    }
}

mod ledger {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(
        remote = "LedgerParams",
        rename_all = "kebab-case",
        deny_unknown_fields
    )]
    pub(super) struct LedgerParamsDef {
        #[serde(default = "default_directory")]
        directory: PathBuf,
        #[serde(default = "default_size_limit")]
        size_limit: u64,
    }

    pub(super) fn default_directory() -> PathBuf {
        PathBuf::from(consts::DEFAULT_ENGINE_LEDGER_DIRECTORY)
    }

    pub(super) const fn default_size_limit() -> u64 {
        consts::DEFAULT_LEDGER_SIZE
    }
}
