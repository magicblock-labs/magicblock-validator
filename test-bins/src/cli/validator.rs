use clap::Parser;
use isocountry::CountryCode;
use magicblock_config::ValidatorConfig;

#[derive(Parser, Debug, Clone)]
pub struct ValidatorConfigArgs {
    /// Duration of a slot in milliseconds.
    #[arg(
        long = "validator-millis-per-slot",
        default_value = "50",
        env = "VALIDATOR_MILLIS_PER_SLOT"
    )]
    pub millis_per_slot: u64,

    /// Whether to verify transaction signatures.
    #[arg(
        long = "validator-sigverify",
        default_value = "true",
        env = "VALIDATOR_SIG_VERIFY"
    )]
    pub sigverify: bool,

    /// Fully Qualified Domain Name. If specified it will also register ER on chain
    #[arg(long = "validator-fdqn", env = "VALIDATOR_FDQN")]
    pub fdqn: Option<String>,

    /// Base fees for transactions.
    #[arg(long = "validator-base-fees", env = "VALIDATOR_BASE_FEES")]
    pub base_fees: Option<u64>,

    /// Uses alpha2 country codes following https://en.wikipedia.org/wiki/ISO_3166-1
    #[arg(
        long = "validator-country-code",
        default_value = "US",
        env = "VALIDATOR_COUNTRY_CODE",
        value_parser = country_code_parser
    )]
    pub country_code: CountryCode,
}

impl From<ValidatorConfigArgs> for ValidatorConfig {
    fn from(value: ValidatorConfigArgs) -> Self {
        ValidatorConfig {
            country_code: value.country_code,
            base_fees: value.base_fees,
            fdqn: value.fdqn,
            millis_per_slot: value.millis_per_slot,
            sigverify: value.sigverify,
        }
    }
}

fn country_code_parser(s: &str) -> Result<CountryCode, String> {
    CountryCode::for_alpha2(s)
        .map_err(|e| format!("Invalid country code: {}", e))
}
