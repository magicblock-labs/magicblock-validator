use clap::Args;
use isocountry::CountryCode;
use magicblock_config_macro::{clap_from_serde, clap_prefix};
use serde::{Deserialize, Serialize};

#[clap_prefix("validator")]
#[clap_from_serde]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args)]
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

    /// By default FQDN is set tp None.
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

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            millis_per_slot: default_millis_per_slot(),
            sigverify: default_sigverify(),
            fqdn: default_fqdn(),
            base_fees: default_base_fees(),
            country_code: default_country_code(),
        }
    }
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
