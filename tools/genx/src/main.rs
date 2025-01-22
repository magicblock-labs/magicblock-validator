use std::path::PathBuf;

use clap::{Parser, Subcommand};
use test_validator::TestValidatorConfig;
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
    }
}
