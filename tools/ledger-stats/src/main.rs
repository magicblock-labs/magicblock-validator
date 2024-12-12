use magicblock_ledger::Ledger;
use std::path::{Path, PathBuf};
use structopt::StructOpt;

mod counts;
mod transactions;

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
            short = "l",
            parse(from_flag),
            help = "Show successful transactions, default: false"
        )]
        success: bool,
        #[structopt(long, short, help = "Start slot")]
        start: Option<u64>,
        #[structopt(long, short, help = "End slot")]
        end: Option<u64>,
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
            transactions::print_transactions(
                &open_ledger(&ledger_path),
                start,
                end,
                success,
            );
        }
    }
}

fn open_ledger(ledger_path: &Path) -> Ledger {
    Ledger::open(ledger_path).expect("Failed to open ledger")
}
