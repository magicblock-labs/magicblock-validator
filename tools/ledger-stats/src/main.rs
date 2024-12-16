use std::{path::PathBuf, str::FromStr};

use crate::utils::open_ledger;
use solana_sdk::pubkey::Pubkey;
use structopt::StructOpt;

mod account;
mod accounts;
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
            short = "u",
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
    #[structopt(name = "accounts", about = "Account details")]
    Accounts {
        #[structopt(parse(from_os_str))]
        ledger_path: PathBuf,
        #[structopt(
            long,
            short,
            parse(from_os_str),
            help = "Column by which to sort accounts",
            default_value = "Pubkey"
        )]
        sort: accounts::SortAccounts,
        #[structopt(long, short, help = "Filter by account owner")]
        owner: Option<String>,
        #[structopt(long, short, help = "Show rent epoch", parse(from_flag))]
        rent_epoch: bool,
        #[structopt(
            long,
            short,
            help = "Filter accounts by specified criteria (comma-separated). PDAs are off-curve",
            possible_values = &["on", "on-curve", "off", "off-curve", "executable", "non-executable"],
            multiple = true,
            use_delimiter = true
        )]
        filter: Vec<String>,
        #[structopt(
            long,
            short,
            help = "Print count instead of account details",
            parse(from_flag)
        )]
        count: bool,
    },
    #[structopt(
        name = "account",
        about = "Specific Account Details including Data"
    )]
    Account {
        #[structopt(parse(from_os_str))]
        ledger_path: PathBuf,
        #[structopt(help = "Pubkey of the account")]
        pubkey: String,
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
        Accounts {
            ledger_path,
            rent_epoch,
            sort,
            owner,
            filter,
            count,
        } => {
            let owner = owner.map(|owner| {
                Pubkey::from_str(&owner).expect("Invalid owner filter pubkey")
            });
            let filters = accounts::FilterAccounts::from_strings(&filter);
            accounts::print_accounts(
                &open_ledger(&ledger_path),
                sort,
                owner,
                &filters,
                rent_epoch,
                count,
            );
        }
        Account {
            ledger_path,
            pubkey,
        } => {
            let ledger = open_ledger(&ledger_path);
            let pubkey = Pubkey::from_str(&pubkey).expect("Invalid pubkey");
            account::print_account(&ledger, &pubkey);
        }
    }
}
