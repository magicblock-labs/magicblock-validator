use magicblock_accounts_db::AccountsDb;
use magicblock_core::traits::AccountsBank;
use num_format::{Locale, ToFormattedString};
use pretty_hex::*;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use tabular::{Row, Table};

pub fn print_account(db: &AccountsDb, pubkey: &Pubkey) {
    let account = db.get_account(pubkey).expect("Account not found");
    let oncurve = pubkey.is_on_curve();

    println!("{} at slot: {}", pubkey, db.slot());
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
                    .with_cell(account.owner().to_string()),
            )
            .with_row(
                Row::new().with_cell("Lamports").with_cell(
                    account.lamports().to_formatted_string(&Locale::en),
                ),
            )
            .with_row(
                Row::new()
                    .with_cell("Executable")
                    .with_cell(account.executable().to_string()),
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
                account.rent_epoch().to_formatted_string(&Locale::en),
            ));

    let data = if !account.data().is_empty() {
        let hex = format!(
            "{:?}",
            account.data().hex_conf(HexConfig {
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
