use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ValidatorConfig {
    #[serde(default = "default_millis_per_slot")]
    pub millis_per_slot: u64,
}

fn default_millis_per_slot() -> u64 {
    50
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            millis_per_slot: default_millis_per_slot(),
        }
    }
}
