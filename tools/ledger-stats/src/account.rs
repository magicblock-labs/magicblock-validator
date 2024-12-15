use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};
use pretty_hex::*;
use solana_sdk::{account::ReadableAccount, pubkey::Pubkey};
use tabular::{Row, Table};

use crate::utils::accounts_storage_from_ledger;

pub fn print_account(ledger: &Ledger, pubkey: &Pubkey) {
    let (storage, slot) = accounts_storage_from_ledger(ledger);
    let account = storage
        .all_accounts()
        .into_iter()
        .find(|acc| acc.pubkey() == pubkey)
        .expect("Account not found");

    let lamports = account.lamports();
    let owner = account.owner();
    let executable = account.executable();
    let rent_epoch = account.rent_epoch();
    let data = account.data();
    let data_len = data.len();
    let oncurve = pubkey.is_on_curve();

    eprintln!("{} at slot: {}", pubkey, slot);
    let table = Table::new("{:<}  {:>}")
        .with_row(Row::new().with_cell("Column").with_cell("Value"))
        .with_row(
            Row::new()
                .with_cell("=========================")
                .with_cell("=============="),
        )
        .with_row(Row::new().with_cell("Pubkey").with_cell(pubkey.to_string()))
        .with_row(Row::new().with_cell("Owner").with_cell(owner.to_string()))
        .with_row(
            Row::new()
                .with_cell("Lamports")
                .with_cell(lamports.to_formatted_string(&Locale::en)),
        )
        .with_row(
            Row::new()
                .with_cell("Executable")
                .with_cell(executable.to_string()),
        )
        .with_row(
            Row::new()
                .with_cell("Data (Bytes)")
                .with_cell(data_len.to_formatted_string(&Locale::en)),
        )
        .with_row(Row::new().with_cell("Curve").with_cell(if oncurve {
            "On"
        } else {
            "Off"
        }))
        .with_row(
            Row::new()
                .with_cell("RentEpoch")
                .with_cell(rent_epoch.to_formatted_string(&Locale::en)),
        );

    let data = if data_len > 0 {
        let hex = format!(
            "{:?}",
            data.hex_conf(HexConfig {
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
