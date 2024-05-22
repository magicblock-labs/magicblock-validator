use serde::{Deserialize, Serialize};

mod accounts;
pub use accounts::*;

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ValidatorConfig {
    #[serde(default)]
    pub accounts: AccountsConfig,
}
