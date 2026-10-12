use serde::{Deserialize, Serialize};

use crate::consts;

/// Strategy for committing transactions back to the base chain
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct CommittorConfig {
    /// The compute unit price (in micro-lamports) to set for commit transactions.
    /// Higher values increase inclusion priority on base chain.
    pub compute_unit_price: u64,
}

impl Default for CommittorConfig {
    fn default() -> Self {
        Self {
            compute_unit_price: consts::DEFAULT_COMPUTE_UNIT_PRICE,
        }
    }
}
