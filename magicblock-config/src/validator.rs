use country_codes::CountryCode;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatorConfig {
    #[serde(default = "default_millis_per_slot")]
    pub millis_per_slot: u64,

    /// By default the validator will verify transaction signature.
    /// This can be disabled by setting [Self::sigverify] to `false`.
    #[serde(default = "default_sigverify")]
    pub sigverify: bool,

    /// By default validator will register or sync itself on chain.
    /// This can be disabled by setting [Self::register_on_chain] to `false`.
    #[serde(default = "default_register_on_chain")]
    pub register_on_chain: bool,

    #[serde(default = "default_base_fees")]
    pub base_fees: Option<u64>,

    /// Uses i32 country codes following https://en.wikipedia.org/wiki/ISO_3166-1
    /// default: 840 (US)
    #[serde(
        default = "default_country_code",
        serialize_with = "serialize_country_code",
        deserialize_with = "deserialize_country_code"
    )]
    pub country_code: CountryCode,
}

fn default_millis_per_slot() -> u64 {
    50
}

fn default_sigverify() -> bool {
    true
}

fn default_register_on_chain() -> bool {
    true
}

fn default_base_fees() -> Option<u64> {
    None
}

fn default_country_code() -> CountryCode {
    country_codes::from_alpha2("US").unwrap()
}

fn serialize_country_code<S>(
    code: &CountryCode,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_i32(code.numeric)
}

fn deserialize_country_code<'de, D>(
    deserializer: D,
) -> Result<CountryCode, D::Error>
where
    D: Deserializer<'de>,
{
    let code = i32::deserialize(deserializer)?;
    let country_code = country_codes::from_numeric(code).ok_or(
        serde::de::Error::custom(format!("Invalid country code: {}", code)),
    )?;

    Ok(country_code)
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            millis_per_slot: default_millis_per_slot(),
            sigverify: default_sigverify(),
            register_on_chain: default_register_on_chain(),
            base_fees: default_base_fees(),
            country_code: default_country_code(),
        }
    }
}
