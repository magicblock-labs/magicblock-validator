use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use num_format::{Locale, ToFormattedString};
use pretty_hex::*;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;

use crate::utils::print_two_col_table;

pub fn print_account(db: &AccountsDb, pubkey: &Pubkey) {
    let account = db.get_account(pubkey).expect("Account not found");
    let oncurve = pubkey.is_on_curve();

    println!("{} at slot: {}", pubkey, db.slot());
    let rows = vec![
        ("Pubkey".to_string(), pubkey.to_string()),
        ("Owner".to_string(), account.owner().to_string()),
        (
            "Lamports".to_string(),
            account.lamports().to_formatted_string(&Locale::en),
        ),
        ("Executable".to_string(), account.executable().to_string()),
        (
            "Data (Bytes)".to_string(),
            account.data().len().to_formatted_string(&Locale::en),
        ),
        (
            "Curve".to_string(),
            if oncurve { "On" } else { "Off" }.to_string(),
        ),
        (
            "RentEpoch".to_string(),
            account.rent_epoch().to_formatted_string(&Locale::en),
        ),
    ];

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
    print_two_col_table(None, ["Column", "Value"], &rows);
    println!();
    println!("{}", data);
}
