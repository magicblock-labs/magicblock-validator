use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use solana_keypair::Keypair;

use crate::{
    consts,
    types::{BindAddress, SerdeKeypair, SerdePubkey},
};

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
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum ReplicationMode {
    /// Validator which does not participate in replication.
    Standalone,
    /// Validator that serves its durable execution stream to authenticated followers.
    Primary {
        /// TCP address on which followers connect.
        #[serde(rename = "bind-address")]
        bind_address: BindAddress,
        /// Local identities permitted to follow this validator.
        #[serde(rename = "allowed-followers")]
        allowed_followers: Vec<SerdePubkey>,
    },
    /// Validator that follows an authenticated upstream execution stream.
    Replica {
        /// TCP address of the immediate upstream validator.
        #[serde(rename = "upstream-address")]
        upstream_address: SocketAddr,
        /// Identity whose signed replication responses this replica accepts.
        #[serde(rename = "upstream-authority")]
        upstream_authority: SerdePubkey,
    },
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
