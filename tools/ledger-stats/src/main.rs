use std::{collections::HashSet, path::PathBuf, str::FromStr};

use clap::{Parser, Subcommand};
use magicblock_accounts_db::AccountsDb;
use solana_pubkey::Pubkey;

use crate::utils::open_ledger;

mod account;
mod accounts;
mod blockhash;
mod counts;
mod transaction_details;
mod transaction_logs;
mod utils;

#[derive(Debug, Subcommand)]
enum Command {
    #[command(name = "count", about = "Counts of items in ledger columns")]
    Count { ledger_path: PathBuf },
    #[command(name = "log", about = "Transaction logs")]
    Log {
        ledger_path: PathBuf,
        #[arg(
            long,
            short = 'u',
            help = "Show successful transactions, default: false"
        )]
        success: bool,
        #[arg(long, short, help = "Start slot")]
        start: Option<u64>,
        #[arg(long, short, help = "End slot")]
        end: Option<u64>,

        #[arg(
            long,
            short,
            value_delimiter = ',',
            help = "Accounts in transaction"
        )]
        accounts: Vec<String>,
    },
    #[command(name = "sig", about = "Transaction details for signature")]
    Sig {
        ledger_path: PathBuf,
        #[arg(help = "Signature")]
        sig: String,
        #[arg(long, short, help = "Show instruction ascii data")]
        ascii: bool,
    },
    #[command(name = "accounts", about = "Account details")]
    Accounts {
        ledger_path: PathBuf,
        #[arg(
            long,
            short,
            value_enum,
            help = "Column by which to sort accounts",
            default_value_t = accounts::SortAccounts::Pubkey
        )]
        sort: accounts::SortAccounts,
        #[arg(long, short, help = "Filter by account owner")]
        owner: Option<String>,
        #[arg(long, short, help = "Show rent epoch")]
        rent_epoch: bool,
        #[arg(
            long,
            short,
            help = "Filter accounts by specified criteria (comma-separated). PDAs are off-curve",
            value_enum,
            value_delimiter = ','
        )]
        filter: Vec<accounts::FilterAccounts>,
        #[arg(long, short, help = "Print count instead of account details")]
        count: bool,
    },
    #[command(
        name = "account",
        about = "Specific Account Details including Data"
    )]
    Account {
        ledger_path: PathBuf,
        #[arg(help = "Pubkey of the account")]
        pubkey: String,
    },
    Blockhash {
        ledger_path: PathBuf,
        #[arg(
            long,
            short,
            help = "Prints the highest slot and blockhash for which a blockhash was recorded",
            value_enum
        )]
        query: blockhash::BlockhashQuery,
    },
}

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn main() {
    let args = Cli::parse();

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
            accounts,
        } => {
            let accounts = (!accounts.is_empty()).then(|| {
                accounts
                    .iter()
                    .map(|account| {
                        Pubkey::from_str(account)
                            .expect("Invalid account pubkey")
                    })
                    .collect::<HashSet<_>>()
            });
            transaction_logs::print_transaction_logs(
                &open_ledger(&ledger_path),
                start,
                end,
                accounts,
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
            accounts::FilterAccounts::sanitize(&filter);
            accounts::print_accounts(
                &AccountsDb::open(&ledger_path)
                    .expect("adb couldn't be opened"),
                sort,
                owner,
                &filter,
                rent_epoch,
                count,
            );
        }
        Account {
            ledger_path,
            pubkey,
        } => {
            let adb =
                AccountsDb::open(&ledger_path).expect("adb couldn't be opened");
            let pubkey = Pubkey::from_str(&pubkey).expect("Invalid pubkey");
            account::print_account(&adb, &pubkey);
        }
        Blockhash { ledger_path, query } => {
            blockhash::print_blockhash_details(
                &open_ledger(&ledger_path),
                query,
            );
        }
    }
}
