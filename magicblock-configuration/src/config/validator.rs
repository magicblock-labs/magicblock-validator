// src/config/validator.rs
use crate::{consts, types::SerdeKeypair};
use serde::{Deserialize, Serialize};

/// Configuration for the validator's core behavior and identity.
#[derive(Deserialize, Serialize, Debug)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatorConfig {
    /// The minimum fee (in lamports) required to process a transaction.
    /// Defaults to 100 lamports.
    pub basefee: u64,

    /// The validator's identity keypair, encoded in Base58.
    pub keypair: SerdeKeypair,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            basefee: consts::DEFAULT_BASE_FEE,
            keypair: SerdeKeypair(solana_keypair::Keypair::from_base58_string(
                consts::DEFAULT_VALIDATOR_KEYPAIR,
            )),
        }
    }
}
