use std::path::PathBuf;

use clap::Parser;
use solana_sdk::signature::Keypair;

use crate::EphemeralConfig;

#[derive(Debug, Clone, Parser)]
pub struct MagicBlockConfig {
    #[clap(help = "Path to the config file")]
    pub config_file: Option<PathBuf>,

    #[clap(
        short,
        long,
        help = "The keypair to use for the validator. DO NOT PROVIDE THIS VALUE VIA THE CLI IN PROD! The default keypair has the pubkey mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev.",
        env = "VALIDATOR_KEYPAIR",
        default_value_t = default_keypair()
    )]
    pub validator_keypair: String,

    #[clap(
        long,
        help = "The comma separated list of geyser cache features to disable. Valid values are 'accounts' and 'transactions'.",
        env = "GEYSER_CACHE_DISABLE",
        default_value = "(accounts,transactions)"
    )]
    pub geyser_cache_disable: String,

    #[clap(
        long,
        help = "The comma separated list of geyser notifications features to disable. Valid values are 'accounts' and 'transactions'.",
        env = "GEYSER_DISABLE",
        default_value = "(accounts,transactions)"
    )]
    pub geyser_disable: String,

    #[clap(flatten)]
    pub config: EphemeralConfig,
}

impl MagicBlockConfig {
    pub fn validator_keypair(&self) -> Keypair {
        Keypair::from_base58_string(&self.validator_keypair)
    }
}

fn default_keypair() -> String {
    bs58::encode(vec![
        7, 83, 184, 55, 200, 223, 238, 137, 166, 244, 107, 126, 189, 16, 194,
        36, 228, 68, 43, 143, 13, 91, 3, 81, 53, 253, 26, 36, 50, 198, 40, 159,
        11, 80, 9, 208, 183, 189, 108, 200, 89, 77, 168, 76, 233, 197, 132, 22,
        21, 186, 202, 240, 105, 168, 157, 64, 233, 249, 100, 104, 210, 41, 83,
        87,
    ])
    .into_string()
}
