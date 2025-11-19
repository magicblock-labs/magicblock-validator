// src/config/validator.rs
use crate::{consts, types::SerdeKeypair};
use clap::Parser;
use serde::{Deserialize, Serialize};

/// Configuration for the validator's core behavior and identity.
#[derive(Parser, Deserialize, Serialize, Debug)]
#[serde(default, rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub struct ValidatorConfig {
    /// The minimum fee (in lamports) required to process a transaction.
    /// Defaults to 100 lamports.
    #[arg(long, default_value = consts::DEFAULT_BASE_FEE_STR)]
    pub basefee: u64,

    /// The validator's identity keypair, encoded in Base58.
    #[arg(long, short, default_value = consts::DEFAULT_VALIDATOR_KEYPAIR)]
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
