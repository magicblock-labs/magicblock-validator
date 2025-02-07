use magicblock_accounts_db::utils::find_account;
use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};
use pretty_hex::*;
use solana_sdk::{
    account::{Account, ReadableAccount},
    pubkey::Pubkey,
};
use tabular::{Row, Table};

use crate::utils::accounts_storage_from_ledger;

pub fn print_account(ledger: &Ledger, pubkey: &Pubkey) {
    let (storage, slot) = accounts_storage_from_ledger(ledger);
    let account = find_account(&storage, |account| {
        (account.pubkey() == pubkey).then_some(Account {
            lamports: account.lamports(),
            owner: *account.owner(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
            data: account.data().to_vec(), // it pains me a lot to call to_vec
        })
    })
    .expect("Account not found");
    let oncurve = pubkey.is_on_curve();

    println!("{} at slot: {}", pubkey, slot);
    let table =
        Table::new("{:<}  {:>}")
            .with_row(Row::new().with_cell("Column").with_cell("Value"))
            .with_row(
                Row::new()
                    .with_cell("=========================")
                    .with_cell("=============="),
            )
            .with_row(
                Row::new().with_cell("Pubkey").with_cell(pubkey.to_string()),
            )
            .with_row(
                Row::new()
                    .with_cell("Owner")
                    .with_cell(account.owner.to_string()),
            )
            .with_row(
                Row::new().with_cell("Lamports").with_cell(
                    account.lamports.to_formatted_string(&Locale::en),
                ),
            )
            .with_row(
                Row::new()
                    .with_cell("Executable")
                    .with_cell(account.executable.to_string()),
            )
            .with_row(Row::new().with_cell("Data (Bytes)").with_cell(
                account.data().len().to_formatted_string(&Locale::en),
            ))
            .with_row(Row::new().with_cell("Curve").with_cell(if oncurve {
                "On"
            } else {
                "Off"
            }))
            .with_row(Row::new().with_cell("RentEpoch").with_cell(
                account.rent_epoch.to_formatted_string(&Locale::en),
            ));

    let data = if !account.data().is_empty() {
        let hex = format!(
            "{:?}",
            account.data.hex_conf(HexConfig {
                width: 16,
                group: 4,
                ascii: true,
                ..Default::default()
            })
        );
        hex
    } else {
        "".to_string()
    };
    println!("{}\n", table);
    println!("{}", data);
}
