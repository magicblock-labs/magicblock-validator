// src/config/validator.rs
use serde::{Deserialize, Serialize};
use solana_keypair::Keypair;

use crate::{consts, types::SerdeKeypair};

/// Configuration for the validator's core behavior and identity.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatorConfig {
    /// The minimum fee (in lamports) required to process a transaction.
    pub basefee: u64,

    /// The validator's identity keypair, encoded in Base58.
    pub keypair: SerdeKeypair,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        let keypair =
            Keypair::from_base58_string(consts::DEFAULT_VALIDATOR_KEYPAIR);
        Self {
            basefee: consts::DEFAULT_BASE_FEE,
            keypair: SerdeKeypair(keypair),
        }
    }
}
