use std::str::FromStr;

use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};
use pretty_hex::*;
use solana_message::VersionedMessage;
use solana_signature::Signature;
use solana_transaction_status::ConfirmedTransactionWithStatusMeta;

use crate::utils::{print_two_col_table, render_logs};

pub(crate) fn print_transaction_details(
    ledger: &Ledger,
    sig: &str,
    ix_data_ascii: bool,
) {
    let sig = Signature::from_str(sig).expect("Invalid signature");
    let (_slot, status_meta) = match ledger
        .get_transaction_status(sig, u64::MAX)
        .expect("Failed to get transaction status")
    {
        Some(val) => val,
        None => {
            eprintln!("Transaction status not found");
            return;
        }
    };

    let status = match &status_meta.status {
        Ok(_) => "Ok".to_string(),
        Err(err) => format!("{:?}", err),
    };

    let pre_balances = status_meta
        .pre_balances
        .iter()
        .map(|b| b.to_formatted_string(&Locale::en))
        .collect::<Vec<_>>()
        .join(" | ");

    let post_balances = status_meta
        .post_balances
        .iter()
        .map(|b| b.to_formatted_string(&Locale::en))
        .collect::<Vec<_>>()
        .join(" | ");

    let inner_instructions = status_meta
        .inner_instructions
        .as_ref()
        .map_or(0, |i| i.len());

    let pre_token_balances =
        status_meta.pre_token_balances.as_ref().map_or_else(
            || "None".to_string(),
            |b| {
                if b.is_empty() {
                    "None".to_string()
                } else {
                    {
                        b.iter()
                            .map(|b| b.ui_token_amount.amount.to_string())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    }
                }
            },
        );

    let post_token_balances =
        status_meta.post_token_balances.as_ref().map_or_else(
            || "None".to_string(),
            |b| {
                if b.is_empty() {
                    "None".to_string()
                } else {
                    {
                        b.iter()
                            .map(|b| b.ui_token_amount.amount.to_string())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    }
                }
            },
        );

    let rewards = status_meta.rewards.as_ref().map_or_else(
        || "None".to_string(),
        |r| {
            if r.is_empty() {
                "None".to_string()
            } else {
                {
                    r.iter()
                        .map(|r| r.lamports.to_formatted_string(&Locale::en))
                        .collect::<Vec<_>>()
                        .join(" | ")
                }
            }
        },
    );

    let return_data = status_meta
        .return_data
        .as_ref()
        .map_or("None".to_string(), |d| {
            d.data.len().to_formatted_string(&Locale::en)
        });

    let compute_units_consumed =
        status_meta.compute_units_consumed.map_or(0, |c| c as usize);

    let rows = vec![
        ("Status".to_string(), status),
        ("Fee".to_string(), status_meta.fee.to_string()),
        ("Pre-balances".to_string(), pre_balances),
        ("Post-balances".to_string(), post_balances),
        (
            "Inner Instructions".to_string(),
            inner_instructions.to_string(),
        ),
        ("Pre-token Balances".to_string(), pre_token_balances),
        ("Post-token Balances".to_string(), post_token_balances),
        ("Rewards".to_string(), rewards),
        (
            "Loaded Addresses".to_string(),
            format!(
                "writable: {}, readonly: {}",
                status_meta.loaded_addresses.writable.len(),
                status_meta.loaded_addresses.readonly.len()
            ),
        ),
        ("Return Data".to_string(), return_data),
        (
            "Compute Units Consumed".to_string(),
            compute_units_consumed.to_string(),
        ),
    ];
    print_two_col_table(Some("Transaction Status"), ["Field", "Value"], &rows);

    match status_meta.log_messages {
        None => {}
        Some(logs) => {
            println!(
                "\n++++ Transaction Logs ++++\n{}",
                render_logs(&logs, "  ")
            );
        }
    }

    let tx = ledger
        .get_complete_transaction(sig, u64::MAX)
        .expect("Failed to get transaction");

    if let Some(ConfirmedTransactionWithStatusMeta {
        tx_with_meta,
        block_time,
        ..
    }) = tx
    {
        if let VersionedMessage::V0(message) =
            tx_with_meta.get_transaction().message
        {
            let rows = vec![
                (
                    "num_required_signatures".to_string(),
                    message.header.num_required_signatures.to_string(),
                ),
                (
                    "num_readonly_signed_accounts".to_string(),
                    message.header.num_readonly_signed_accounts.to_string(),
                ),
                (
                    "num_readonly_unsigned_accounts".to_string(),
                    message.header.num_readonly_unsigned_accounts.to_string(),
                ),
                (
                    "block_time".to_string(),
                    block_time.unwrap_or_default().to_string(),
                ),
            ];
            print_two_col_table(Some("Transaction"), ["Field", "Value"], &rows);

            println!("++++ Account Keys ++++\n");
            for account_key in &message.account_keys {
                println!("  • {}", account_key);
            }

            println!("\n++++ Instructions ++++\n");
            for (idx, instruction) in message.instructions.iter().enumerate() {
                let program_id =
                    message.account_keys[instruction.program_id_index as usize];
                println!("#{} Program ID: {}", idx + 1, program_id);

                println!("\n  Accounts:");
                for account_index in &instruction.accounts {
                    let account_key =
                        message.account_keys[*account_index as usize];
                    println!("    • {}", account_key);
                }

                print!("\n  Instruction Data ");
                let hex = format!(
                    "{:?}",
                    instruction.data.hex_conf(HexConfig {
                        width: 16,
                        group: 4,
                        ascii: ix_data_ascii,
                        ..Default::default()
                    })
                );
                let hex_indented = hex.lines().collect::<Vec<_>>().join("\n  ");
                println!("{}", hex_indented);
            }
        }
    }
}
