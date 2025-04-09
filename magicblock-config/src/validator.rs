use isocountry::CountryCode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatorConfig {
    #[serde(default = "default_millis_per_slot")]
    pub millis_per_slot: u64,

    /// By default the validator will verify transaction signature.
    /// This can be disabled by setting [Self::sigverify] to `false`.
    #[serde(default = "default_sigverify")]
    pub sigverify: bool,

    /// By default FDQN is set tp None.
    /// If specified it will also register ER on chain
    #[serde(default = "default_fdqn")]
    pub fdqn: Option<String>,

    #[serde(default = "default_base_fees")]
    pub base_fees: Option<u64>,

    /// Uses alpha2 country codes following https://en.wikipedia.org/wiki/ISO_3166-1
    /// default: "US"
    #[serde(default = "default_country_code")]
    pub country_code: CountryCode,
}

fn default_millis_per_slot() -> u64 {
    50
}

fn default_sigverify() -> bool {
    true
}

fn default_fdqn() -> Option<String> {
    None
}

fn default_base_fees() -> Option<u64> {
    None
}

fn default_country_code() -> CountryCode {
    CountryCode::for_alpha2("US").unwrap()
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            millis_per_slot: default_millis_per_slot(),
            sigverify: default_sigverify(),
            fdqn: default_fdqn(),
            base_fees: default_base_fees(),
            country_code: default_country_code(),
        }
    }
}
