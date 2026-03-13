// src/config/validator.rs
use serde::{Deserialize, Serialize};
use solana_keypair::Keypair;
use url::Url;

use crate::{consts, types::SerdeKeypair};

/// Configuration for the validator's core behavior and identity.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatorConfig {
    /// The minimum fee (in lamports) required to process a transaction.
    pub basefee: u64,

    /// The validator's identity keypair, encoded in Base58.
    pub keypair: SerdeKeypair,

    /// Replication role: Primary accepts client transactions, Replica replays from Primary.
    pub replication_mode: ReplicationMode,
}

/// Defines the validator's role in a replication setup.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum ReplicationMode {
    // Validator which doesn't participate in replication
    Standalone,
    /// Validator which participates in replication: acting as either a primary or replicator
    StandBy(Url),
    /// Validator which participates in replication only as replicator (no takeover)
    ReplicatOnly(Url),
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        let keypair =
            Keypair::from_base58_string(consts::DEFAULT_VALIDATOR_KEYPAIR);
        Self {
            basefee: consts::DEFAULT_BASE_FEE,
            keypair: SerdeKeypair(keypair),
            replication_mode: ReplicationMode::Standalone,
        }
    }
}

impl ReplicationMode {
    /// Returns the remote URL if this node participates in replication.
    /// Returns `None` for `Standalone` mode.
    pub fn remote(&self) -> Option<Url> {
        match self {
            Self::Standalone => None,
            Self::StandBy(u) | Self::ReplicatOnly(u) => Some(u.clone()),
        }
    }
}
