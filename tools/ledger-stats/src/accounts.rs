use std::ffi::OsStr;

use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};
use solana_sdk::account::ReadableAccount;
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;
use structopt::StructOpt;
use tabular::{Row, Table};

use crate::utils::accounts_storage_from_ledger;

// -----------------
// SortAccounts
// -----------------
#[derive(Debug, Default, StructOpt)]
pub enum SortAccounts {
    #[default]
    Pubkey,
    Owner,
    Lamports,
    Executable,
    DataLen,
    RentEpoch,
}

impl From<&OsStr> for SortAccounts {
    fn from(s: &OsStr) -> Self {
        use SortAccounts::*;
        let lower_case = s.to_str().unwrap().to_lowercase();
        let s = lower_case.as_str();
        if s.starts_with('o') {
            Owner
        } else if s.starts_with('l') {
            Lamports
        } else if s.starts_with('e') {
            Executable
        } else if s.starts_with('d') {
            DataLen
        } else if s.starts_with('r') {
            RentEpoch
        } else {
            Pubkey
        }
    }
}

// -----------------
// FilterAccounts
// -----------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterAccounts {
    Executable,
    NonExecutable,
    OnCurve,
    OffCurve,
}

impl From<&str> for FilterAccounts {
    fn from(s: &str) -> Self {
        use FilterAccounts::*;
        match s {
            "executable" => Executable,
            "non-executable" => NonExecutable,
            "curve" => OnCurve,
            "pda" => OffCurve,
            _ => panic!("Invalid filter {}", s),
        }
    }
}

impl FilterAccounts {
    pub(crate) fn from_strings(s: &[String]) -> Vec<Self> {
        let filters =
            s.iter().map(|s| Self::from(s.as_str())).collect::<Vec<_>>();
        Self::sanitize(&filters);
        filters
    }

    fn sanitize(filters: &[Self]) {
        if filters.contains(&Self::OnCurve) && filters.contains(&Self::OffCurve)
        {
            panic!("Cannot filter by both curve and pda");
        }
        if filters.contains(&Self::Executable)
            && filters.contains(&Self::NonExecutable)
        {
            panic!("Cannot filter by both executable and non-executable");
        }
    }
}

// -----------------
// AccountInfo
// -----------------
struct AccountInfo<'a> {
    /// Pubkey of the account
    pub pubkey: &'a Pubkey,
    /// lamports in the account
    pub lamports: u64,
    /// the epoch at which this account will next owe rent
    pub rent_epoch: Epoch,
    /// the program that owns this account. If executable, the program that loads this account.
    pub owner: &'a Pubkey,
    /// this account's data contains a loaded program (and is now read-only)
    pub executable: bool,
    /// the data in this account
    pub data: &'a [u8],
}

pub fn print_accounts(
    ledger: &Ledger,
    sort: SortAccounts,
    owner: Option<Pubkey>,
    filters: &[FilterAccounts],
    print_rent_epoch: bool,
    count: bool,
) {
    let storage = accounts_storage_from_ledger(ledger);

    let mut accounts = {
        let all = storage.all_accounts();
        all.into_iter()
            .filter(|acc| {
                if !owner.map_or(true, |owner| acc.owner().eq(&owner)) {
                    return false;
                }
                if filters.contains(&FilterAccounts::Executable)
                    && !acc.executable()
                {
                    return false;
                }
                if filters.contains(&FilterAccounts::NonExecutable)
                    && acc.executable()
                {
                    return false;
                }
                if filters.contains(&FilterAccounts::OnCurve)
                    && !acc.owner().is_on_curve()
                {
                    return false;
                }
                if filters.contains(&FilterAccounts::OffCurve)
                    && acc.owner().is_on_curve()
                {
                    return false;
                }

                true
            })
            .collect::<Vec<_>>()
    };
    accounts.sort_by(|a, b| {
        use SortAccounts::*;
        match sort {
            Pubkey => a.pubkey().cmp(b.pubkey()),
            Owner => a.owner().cmp(b.owner()),
            Lamports => a.lamports().cmp(&b.lamports()),
            Executable => a.executable().cmp(&b.executable()),
            DataLen => a.data().len().cmp(&b.data().len()),
            RentEpoch => a.rent_epoch().cmp(&b.rent_epoch()),
        }
    });

    if count {
        if let Some(owner) = owner {
            println!("Total accounts owned by '{}': {}", owner, accounts.len());
        } else {
            println!("Total accounts: {}", accounts.len());
        }
        return;
    }

    let table_alignment = if print_rent_epoch {
        "{:<}  {:<}  {:>}  {:<}  {:>}  {:>}"
    } else {
        "{:<}  {:<}  {:>}  {:<}  {:>}"
    };
    let mut table = Table::new(table_alignment);
    let mut row = Row::new()
        .with_cell("Pubkey")
        .with_cell("Owner")
        .with_cell("Lamports")
        .with_cell("Executable")
        .with_cell("Data (Bytes)");
    if print_rent_epoch {
        row.add_cell("Rent Epoch");
    }
    table.add_row(row);

    fn add_row(table: &mut Table, meta: AccountInfo, include_rent_epoch: bool) {
        let mut row = Row::new()
            .with_cell(meta.pubkey.to_string())
            .with_cell(meta.owner.to_string())
            .with_cell(meta.lamports.to_formatted_string(&Locale::en))
            .with_cell(meta.executable)
            .with_cell(meta.data.len());
        if include_rent_epoch {
            row.add_cell(meta.rent_epoch.to_formatted_string(&Locale::en));
        }
        table.add_row(row);
    }

    for acc in accounts {
        add_row(
            &mut table,
            AccountInfo {
                pubkey: acc.pubkey(),
                lamports: acc.lamports(),
                rent_epoch: acc.rent_epoch(),
                owner: acc.owner(),
                executable: acc.executable(),
                data: acc.data(),
            },
            print_rent_epoch,
        );
    }

    println!("{}", table);
}
