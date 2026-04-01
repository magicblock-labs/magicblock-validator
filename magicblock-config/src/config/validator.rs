use std::fmt;

use serde::{Deserialize, Serialize};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use url::Url;

use crate::{
    consts,
    types::{SerdeKeypair, SerdePubkey},
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
#[serde(rename_all = "kebab-case")]
pub enum ReplicationMode {
    // Validator which doesn't participate in replication
    Standalone,
    /// Validator which participates in replication: acting as either a primary or replicator
    StandBy(ReplicationConfig),
    /// Validator which participates in replication only as replicator (no takeover)
    ReplicaOnly(ReplicationConfig),
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct ReplicationConfig {
    pub url: Url,
    pub secret: String,
    pub authority_override: Option<SerdePubkey>,
}

impl fmt::Debug for ReplicationConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplicationConfig")
            .field("url", &self.url)
            .field("secret", &"<redacted>")
            .field("authority_override", &self.authority_override)
            .finish()
    }
}

impl Serialize for ReplicationConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        #[serde(rename_all = "kebab-case")]
        struct Redacted<'a> {
            url: &'a Url,
            secret: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            authority_override: &'a Option<SerdePubkey>,
        }

        Redacted {
            url: &self.url,
            secret: "<redacted>",
            authority_override: &self.authority_override,
        }
        .serialize(serializer)
    }
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
    pub fn config(&self) -> Option<ReplicationConfig> {
        match self {
            Self::Standalone => None,
            Self::StandBy(c) | Self::ReplicaOnly(c) => Some(c.clone()),
        }
    }

    pub fn authority_override(&self) -> Option<Pubkey> {
        if let Self::ReplicaOnly(c) = self {
            return c.authority_override.as_ref().map(|pk| pk.0);
        }
        None
    }
}
