use std::path::{Path, PathBuf};

use magicblock_ledger::Ledger;
use structopt::StructOpt;

mod counts;
mod transaction_details;
mod transaction_logs;
mod utils;

#[derive(Debug, StructOpt)]
enum Command {
    #[structopt(name = "count", about = "Counts of items in ledger columns")]
    Count {
        #[structopt(parse(from_os_str))]
        ledger_path: PathBuf,
    },
    #[structopt(name = "log", about = "Transaction logs")]
    Log {
        #[structopt(parse(from_os_str))]
        ledger_path: PathBuf,
        #[structopt(
            long,
            short = "s",
            parse(from_flag),
            help = "Show successful transactions, default: false"
        )]
        success: bool,
        #[structopt(long, short, help = "Start slot")]
        start: Option<u64>,
        #[structopt(long, short, help = "End slot")]
        end: Option<u64>,
    },
    #[structopt(name = "sig", about = "Transaction details for signature")]
    Sig {
        #[structopt(parse(from_os_str))]
        ledger_path: PathBuf,
        #[structopt(help = "Signature")]
        sig: String,
        #[structopt(
            long,
            short,
            help = "Show instruction ascii data",
            parse(from_flag)
        )]
        ascii: bool,
    },
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(subcommand)]
    command: Command,
}

fn main() {
    let args = Cli::from_args();

    use Command::*;
    match args.command {
        Count { ledger_path } => {
            counts::print_counts(&open_ledger(&ledger_path))
        }
        Log {
            ledger_path,
            success,
            start,
            end,
        } => {
            transaction_logs::print_transaction_logs(
                &open_ledger(&ledger_path),
                start,
                end,
                success,
            );
        }
        Sig {
            ledger_path,
            sig,
            ascii,
        } => {
            let ledger = open_ledger(&ledger_path);
            transaction_details::print_transaction_details(
                &ledger, &sig, ascii,
            );
        }
    }
}

fn open_ledger(ledger_path: &Path) -> Ledger {
    Ledger::open(ledger_path).expect("Failed to open ledger")
}
