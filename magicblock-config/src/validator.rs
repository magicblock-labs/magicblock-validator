use clap::Args;
use isocountry::CountryCode;
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CLAIM_FEES_INTERVAL_SECS: u64 = 3600;

#[clap_prefix("validator")]
#[clap_from_serde]
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args, Mergeable,
)]
#[serde(deny_unknown_fields)]
pub struct ValidatorConfig {
    #[derive_env_var]
    #[arg(help = "The duration of a slot in milliseconds.")]
    #[serde(default = "default_millis_per_slot")]
    pub millis_per_slot: u64,

    /// By default the validator will verify transaction signature.
    /// This can be disabled by setting [Self::sigverify] to `false`.
    #[derive_env_var]
    #[arg(help = "Whether to verify transaction signatures.")]
    #[serde(default = "default_sigverify")]
    pub sigverify: bool,

    /// By default FQDN is set to None.
    /// If specified it will also register ER on chain
    #[derive_env_var]
    #[clap_from_serde_skip] // Skip because it defaults to None
    #[arg(default_value = None, help = "The FQDN to use for the validator.")]
    #[serde(default = "default_fqdn")]
    pub fqdn: Option<String>,

    #[derive_env_var]
    #[clap_from_serde_skip] // Skip because it defaults to None
    #[arg(help = "The base fees to use for the validator.")]
    #[serde(default = "default_base_fees")]
    pub base_fees: Option<u64>,

    /// Uses alpha2 country codes following https://en.wikipedia.org/wiki/ISO_3166-1
    /// default: "US"
    #[derive_env_var]
    #[arg(
        help = "The country code to use for the validator.",
        value_parser = parse_country_code,
        default_value_t = default_country_code()
    )]
    #[serde(default = "default_country_code")]
    pub country_code: CountryCode,

    /// The interval in seconds at which the validator will claim fees.
    /// default: 3600 (1 hour)
    #[derive_env_var]
    #[arg(
        help = "The interval in seconds at which the validator will claim fees."
    )]
    #[serde(default = "default_claim_fees_interval_secs")]
    pub claim_fees_interval_secs: u64,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            millis_per_slot: default_millis_per_slot(),
            sigverify: default_sigverify(),
            fqdn: default_fqdn(),
            base_fees: default_base_fees(),
            country_code: default_country_code(),
            claim_fees_interval_secs: default_claim_fees_interval_secs(),
        }
    }
}

fn default_millis_per_slot() -> u64 {
    50
}

fn default_sigverify() -> bool {
    true
}

fn default_fqdn() -> Option<String> {
    None
}

fn default_base_fees() -> Option<u64> {
    None
}

fn default_country_code() -> CountryCode {
    CountryCode::for_alpha2("US").unwrap()
}

fn default_claim_fees_interval_secs() -> u64 {
    DEFAULT_CLAIM_FEES_INTERVAL_SECS
}

fn parse_country_code(s: &str) -> Result<CountryCode, String> {
    if let Ok(code) = CountryCode::for_alpha2(s) {
        Ok(code)
    } else if s != default_country_code().name() {
        Err(format!("Invalid country code: {s}"))
    } else {
        Ok(default_country_code())
    }
}

#[cfg(test)]
mod tests {
    use magicblock_config_helpers::Merge;

    use super::*;

    #[test]
    fn test_merge_with_default() {
        let mut config = ValidatorConfig {
            millis_per_slot: 5000,
            sigverify: false,
            fqdn: Some("validator.example.com".to_string()),
            base_fees: Some(1000000000),
            country_code: CountryCode::for_alpha2("FR").unwrap(),
            claim_fees_interval_secs: DEFAULT_CLAIM_FEES_INTERVAL_SECS,
        };
        let original_config = config.clone();
        let other = ValidatorConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_merge_default_with_non_default() {
        let mut config = ValidatorConfig::default();
        let other = ValidatorConfig {
            millis_per_slot: 5000,
            sigverify: false,
            fqdn: Some("validator.example.com".to_string()),
            base_fees: Some(1000000000),
            country_code: CountryCode::for_alpha2("FR").unwrap(),
            claim_fees_interval_secs: DEFAULT_CLAIM_FEES_INTERVAL_SECS,
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_merge_non_default() {
        let mut config = ValidatorConfig {
            millis_per_slot: 5001,
            sigverify: false,
            fqdn: Some("validator2.example.com".to_string()),
            base_fees: Some(9999),
            country_code: CountryCode::for_alpha2("DE").unwrap(),
            claim_fees_interval_secs: DEFAULT_CLAIM_FEES_INTERVAL_SECS,
        };
        let original_config = config.clone();
        let other = ValidatorConfig {
            millis_per_slot: 5000,
            sigverify: true,
            fqdn: Some("validator.example.com".to_string()),
            base_fees: Some(1000000000),
            country_code: CountryCode::for_alpha2("FR").unwrap(),
            claim_fees_interval_secs: DEFAULT_CLAIM_FEES_INTERVAL_SECS,
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }
}
