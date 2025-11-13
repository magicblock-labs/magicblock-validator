use std::str::FromStr;

use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};
use pretty_hex::*;
use solana_sdk::{message::VersionedMessage, signature::Signature};
use solana_transaction_status::ConfirmedTransactionWithStatusMeta;
use tabular::{Row, Table};

use crate::utils::render_logs;

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
                if b.is_empty() { "None".to_string() } else { {
                    b.iter()
                        .map(|b| b.ui_token_amount.amount.to_string())
                        .collect::<Vec<_>>()
                        .join(" | ")
                } }
            },
        );

    let post_token_balances =
        status_meta.post_token_balances.as_ref().map_or_else(
            || "None".to_string(),
            |b| {
                if b.is_empty() { "None".to_string() } else { {
                    b.iter()
                        .map(|b| b.ui_token_amount.amount.to_string())
                        .collect::<Vec<_>>()
                        .join(" | ")
                } }
            },
        );

    let rewards = status_meta.rewards.as_ref().map_or_else(
        || "None".to_string(),
        |r| {
            if r.is_empty() { "None".to_string() } else { {
                r.iter()
                    .map(|r| r.lamports.to_formatted_string(&Locale::en))
                    .collect::<Vec<_>>()
                    .join(" | ")
            } }
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

    let table = Table::new("{:<}  {:>}")
        .with_heading("\n++++ Transaction Status ++++\n")
        .with_row(Row::new().with_cell("Field").with_cell("Value"))
        .with_row(
            Row::new()
                .with_cell("=====================")
                .with_cell("====================="),
        )
        .with_row(Row::new().with_cell("Status").with_cell(status))
        .with_row(Row::new().with_cell("Fee").with_cell(status_meta.fee))
        .with_row(Row::new().with_cell("Pre-balances").with_cell(pre_balances))
        .with_row(
            Row::new()
                .with_cell("Post-balances")
                .with_cell(post_balances),
        )
        .with_row(
            Row::new()
                .with_cell("Inner Instructions")
                .with_cell(inner_instructions),
        )
        .with_row(
            Row::new()
                .with_cell("Pre-token Balances")
                .with_cell(pre_token_balances),
        )
        .with_row(
            Row::new()
                .with_cell("Post-token Balances")
                .with_cell(post_token_balances),
        )
        .with_row(Row::new().with_cell("Rewards").with_cell(rewards))
        .with_row(Row::new().with_cell("Loaded Addresses").with_cell(format!(
            "writable: {}, readonly: {}",
            status_meta.loaded_addresses.writable.len(),
            status_meta.loaded_addresses.readonly.len()
        )))
        .with_row(Row::new().with_cell("Return Data").with_cell(return_data))
        .with_row(
            Row::new()
                .with_cell("Compute Units Consumed")
                .with_cell(compute_units_consumed),
        );

    println!("{}", table);

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
            let table = Table::new("{:<}  {:>}")
                .with_heading("\n++++ Transaction ++++\n")
                .with_row(
                    Row::new()
                        .with_cell("num_required_signatures")
                        .with_cell(message.header.num_required_signatures),
                )
                .with_row(
                    Row::new()
                        .with_cell("num_readonly_signed_accounts")
                        .with_cell(message.header.num_readonly_signed_accounts),
                )
                .with_row(
                    Row::new()
                        .with_cell("num_readonly_unsigned_accounts")
                        .with_cell(
                            message.header.num_readonly_unsigned_accounts,
                        ),
                )
                .with_row(
                    Row::new()
                        .with_cell("block_time")
                        .with_cell(block_time.unwrap_or_default().to_string()),
                );

            println!("{}", table);

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
