use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ValidatorConfig {
    #[serde(default = "default_millis_per_slot")]
    pub millis_per_slot: u64,

    /// By default the validator will verify transaction signature.
    /// This can be disabled by setting [Self::sigverify] to `false`.
    #[serde(default = "default_sigverify")]
    pub sigverify: bool,

    /// If a previous ledger is found it is removed before starting the validator
    /// This can be disabled by setting [Self::reset_ledger] to `false`.
    #[serde(default = "default_reset_ledger")]
    pub reset_ledger: bool,
}

fn default_millis_per_slot() -> u64 {
    50
}

fn default_sigverify() -> bool {
    true
}

fn default_reset_ledger() -> bool {
    true
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            millis_per_slot: default_millis_per_slot(),
            sigverify: default_sigverify(),
            reset_ledger: default_reset_ledger(),
        }
    }
}
