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
    #[structopt(name = "tx-fail", about = "Failed transaction detailss")]
    TransactionsFail {
        #[structopt(parse(from_os_str))]
        ledger_path: PathBuf,
    },
    #[structopt(name = "tx-success", about = "Successful transaction details")]
    TransactionsSuccess {
        #[structopt(parse(from_os_str))]
        ledger_path: PathBuf,
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
        TransactionsFail { ledger_path } => {
            transactions::print_transactions(&open_ledger(&ledger_path), false);
        }
        TransactionsSuccess { ledger_path } => {
            transactions::print_transactions(&open_ledger(&ledger_path), true);
        }
    }
}

fn open_ledger(ledger_path: &Path) -> Ledger {
    Ledger::open(ledger_path).expect("Failed to open ledger")
}
