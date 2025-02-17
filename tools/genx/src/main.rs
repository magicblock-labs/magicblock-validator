use std::path::PathBuf;

use clap::{Parser, Subcommand};
use test_validator::TestValidatorConfig;
mod ledger_replay_test;
mod test_validator;

#[derive(Debug, Parser)]
#[command(name = "genx")]
#[command(about = "genx CLI tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Generates a script to run a test validator
    #[command(name = "test-validator")]
    #[command(
        about = "Generates a script to run a test validator",
        long_about = "Example: genx test-validator --rpc-port 7799 --url devnet path/to/ledger"
    )]
    TestValidator {
        ledger_path: Option<PathBuf>,

        #[arg(long)]
        rpc_port: u16,

        #[arg(long)]
        url: String,
    },
    /// Prepares the ledger for testing
    #[command(name = "test-ledger")]
    #[command(
        about = "Prepares the ledger for testing",
        long_about = "(Over)writes the validator keypair"
    )]
    LedgerReplayTest { ledger_path: PathBuf },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::TestValidator {
            ledger_path,
            rpc_port,
            url,
        } => {
            let config = TestValidatorConfig { rpc_port, url };
            test_validator::gen_test_validator_start_script(
                ledger_path.as_ref(),
                config,
            )
        }
        Commands::LedgerReplayTest { ledger_path } => {
            ledger_replay_test::ledger_replay_test(&ledger_path)
        }
    }
}
