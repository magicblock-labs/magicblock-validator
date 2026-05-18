use clap::ValueEnum;
use magicblock_accounts_db::AccountsDb;
use num_format::{Locale, ToFormattedString};
use solana_account::ReadableAccount;
use solana_clock::Epoch;
use solana_pubkey::Pubkey;

// -----------------
// SortAccounts
// -----------------
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum SortAccounts {
    #[default]
    #[value(alias = "p")]
    Pubkey,
    #[value(alias = "o")]
    Owner,
    #[value(alias = "l")]
    Lamports,
    #[value(alias = "e")]
    Executable,
    #[value(alias = "d")]
    DataLen,
    #[value(alias = "r")]
    RentEpoch,
}

// -----------------
// FilterAccounts
// -----------------
#[derive(Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum FilterAccounts {
    Executable,
    NonExecutable,
    #[value(alias = "on")]
    OnCurve,
    #[value(alias = "off")]
    OffCurve,
}

impl FilterAccounts {
    pub(crate) fn sanitize(filters: &[Self]) {
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
struct AccountInfo {
    /// Pubkey of the account
    pub pubkey: Pubkey,
    /// lamports in the account
    pub lamports: u64,
    /// the epoch at which this account will next owe rent
    pub rent_epoch: Epoch,
    /// the program that owns this account. If executable, the program that loads this account.
    pub owner: Pubkey,
    /// this account's data contains a loaded program (and is now read-only)
    pub executable: bool,
    /// the data in this account
    pub data: Vec<u8>,
}

pub fn print_accounts(
    adb: &AccountsDb,
    sort: SortAccounts,
    owner: Option<Pubkey>,
    filters: &[FilterAccounts],
    print_rent_epoch: bool,
    count: bool,
) {
    let mut accounts = {
        let iter = adb.iter_all();
        let all = iter.map(|(pubkey, acc)| AccountInfo {
            pubkey,
            lamports: acc.lamports(),
            rent_epoch: acc.rent_epoch(),
            owner: *acc.owner(),
            executable: acc.executable(),
            data: acc.data().to_vec(),
        });
        all.into_iter()
            .filter(|acc| {
                if !owner.is_none_or(|owner| acc.owner.eq(&owner)) {
                    return false;
                }
                if filters.contains(&FilterAccounts::Executable)
                    && !acc.executable
                {
                    return false;
                }
                if filters.contains(&FilterAccounts::NonExecutable)
                    && acc.executable
                {
                    return false;
                }
                if filters.contains(&FilterAccounts::OnCurve)
                    && !acc.pubkey.is_on_curve()
                {
                    return false;
                }
                if filters.contains(&FilterAccounts::OffCurve)
                    && acc.pubkey.is_on_curve()
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
            Pubkey => a.pubkey.cmp(&b.pubkey),
            Owner => a.owner.cmp(&b.owner),
            Lamports => a.lamports.cmp(&b.lamports),
            Executable => a.executable.cmp(&b.executable),
            DataLen => a.data.len().cmp(&b.data.len()),
            RentEpoch => a.rent_epoch.cmp(&b.rent_epoch),
        }
    });

    let slot = adb.slot();
    if count {
        if let Some(owner) = owner {
            println!(
                "Total accounts at slot {} owned by '{}': {}",
                slot,
                owner,
                accounts.len()
            );
        } else {
            println!("Total accounts at slot {}: {}", slot, accounts.len());
        }
        return;
    }

    let mut headers = vec![
        "Pubkey",
        "Owner",
        "Lamports",
        "Executable",
        "Data(Bytes)",
        "Curve",
    ];
    if print_rent_epoch {
        headers.push("Rent Epoch");
    }
    let rows = accounts
        .into_iter()
        .map(|acc| {
            let mut row = vec![
                acc.pubkey.to_string(),
                acc.owner.to_string(),
                acc.lamports.to_formatted_string(&Locale::en),
                acc.executable.to_string(),
                acc.data.len().to_string(),
                if acc.pubkey.is_on_curve() {
                    "On".to_string()
                } else {
                    "Off".to_string()
                },
            ];
            if print_rent_epoch {
                row.push(acc.rent_epoch.to_formatted_string(&Locale::en));
            }
            row
        })
        .collect::<Vec<_>>();
    println!("Accounts at slot {}", slot);
    print_rows(&headers, &rows);
}

fn print_rows(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }

    print_row(headers.iter().copied(), &widths);
    print_row(widths.iter().map(|width| "-".repeat(*width)), &widths);
    for row in rows {
        print_row(row.iter().map(String::as_str), &widths);
    }
}

fn print_row<'a>(
    cells: impl IntoIterator<Item = impl AsRef<str> + 'a>,
    widths: &[usize],
) {
    for (idx, (cell, width)) in cells.into_iter().zip(widths).enumerate() {
        if idx > 0 {
            print!("  ");
        }
        print!("{:<width$}", cell.as_ref(), width = *width);
    }
    println!();
}
