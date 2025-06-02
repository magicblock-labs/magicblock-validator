use clap::Parser;
use magicblock_config::LedgerConfig;

#[derive(Parser, Debug, Clone)]
pub struct LedgerConfigArgs {
    /// Whether to remove a previous ledger if it exists.
    #[arg(long = "ledger-reset", default_value = "true", env = "LEDGER_RESET")]
    pub reset: bool,
    /// The file system path onto which the ledger should be written at.
    /// If left empty it will be auto-generated to a temporary folder
    #[arg(long = "ledger-path", env = "LEDGER_PATH")]
    pub path: Option<String>,
    /// The size under which it's desired to keep ledger in bytes. Default is 100 GiB
    #[arg(
        long = "ledger-size",
        default_value = "107374182400",
        env = "LEDGER_SIZE"
    )]
    pub size: u64,
}

impl From<LedgerConfigArgs> for LedgerConfig {
    fn from(value: LedgerConfigArgs) -> Self {
        LedgerConfig {
            reset: value.reset,
            path: value.path,
            size: value.size,
        }
    }
}
