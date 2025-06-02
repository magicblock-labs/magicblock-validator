use clap::Parser;
use isocountry::CountryCode;
use magicblock_api::EphemeralConfig;
use magicblock_config::ValidatorConfig;

#[derive(Parser, Debug, Clone)]
pub struct ValidatorConfigArgs {
    /// Duration of a slot in milliseconds.
    #[arg(
        long = "validator-millis-per-slot",
        env = "VALIDATOR_MILLIS_PER_SLOT"
    )]
    pub millis_per_slot: Option<u64>,

    /// Whether to verify transaction signatures.
    #[arg(long = "validator-sigverify", env = "VALIDATOR_SIG_VERIFY")]
    pub sigverify: Option<bool>,

    /// Fully Qualified Domain Name. If specified it will also register ER on chain
    #[arg(long = "validator-fdqn", env = "VALIDATOR_FDQN")]
    pub fdqn: Option<String>,

    /// Base fees for transactions.
    #[arg(long = "validator-base-fees", env = "VALIDATOR_BASE_FEES")]
    pub base_fees: Option<u64>,

    /// Uses alpha2 country codes following https://en.wikipedia.org/wiki/ISO_3166-1
    #[arg(
        long = "validator-country-code",
        env = "VALIDATOR_COUNTRY_CODE",
        value_parser = country_code_parser
    )]
    pub country_code: Option<CountryCode>,
}

impl ValidatorConfigArgs {
    pub fn merge_with_config(&self, config: &mut EphemeralConfig) {
        config.validator = ValidatorConfig {
            millis_per_slot: self
                .millis_per_slot
                .unwrap_or(config.validator.millis_per_slot),
            sigverify: self.sigverify.unwrap_or(config.validator.sigverify),
            fdqn: self.fdqn.clone(),
            base_fees: self.base_fees.or(config.validator.base_fees),
            country_code: self
                .country_code
                .unwrap_or(config.validator.country_code),
        }
    }
}

fn country_code_parser(s: &str) -> Result<CountryCode, String> {
    CountryCode::for_alpha2(s)
        .map_err(|e| format!("Invalid country code: {}", e))
}
