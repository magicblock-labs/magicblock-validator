use clap::Parser;
use magicblock_api::EphemeralConfig;
use magicblock_config::LedgerConfig;

#[derive(Parser, Debug, Clone)]
pub struct LedgerConfigArgs {
    /// Whether to remove a previous ledger if it exists.
    #[arg(long = "ledger-reset", env = "LEDGER_RESET")]
    pub reset: Option<bool>,
    /// The file system path onto which the ledger should be written at.
    /// If left empty it will be auto-generated to a temporary folder
    #[arg(long = "ledger-path", env = "LEDGER_PATH")]
    pub path: Option<String>,
    /// The size under which it's desired to keep ledger in bytes. Default is 100 GiB
    #[arg(long = "ledger-size", env = "LEDGER_SIZE")]
    pub size: Option<u64>,
}

impl LedgerConfigArgs {
    pub fn merge_with_config(&self, config: &mut EphemeralConfig) {
        config.ledger = LedgerConfig {
            reset: self.reset.unwrap_or(config.ledger.reset),
            path: self.path.clone().or(config.ledger.path.clone()),
            size: self.size.unwrap_or(config.ledger.size),
        }
    }
}
