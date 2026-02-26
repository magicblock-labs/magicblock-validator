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
pub enum ReplicationMode {
    /// Primary validator: accepts and executes client transactions.
    Primary,
    /// Replica validator: replays transactions from the primary at the given URL.
    Replica(Url),
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        let keypair =
            Keypair::from_base58_string(consts::DEFAULT_VALIDATOR_KEYPAIR);
        Self {
            basefee: consts::DEFAULT_BASE_FEE,
            keypair: SerdeKeypair(keypair),
            replication_mode: ReplicationMode::Primary,
        }
    }
}
