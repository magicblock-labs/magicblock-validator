use std::collections::HashSet;

use magicblock_ledger::Ledger;
use solana_pubkey::Pubkey;

use crate::utils::render_logs;

pub(crate) fn print_transaction_logs(
    ledger: &Ledger,
    start_slot: Option<u64>,
    end_slot: Option<u64>,
    accounts: Option<HashSet<Pubkey>>,
    success: bool,
) {
    let start_slot = start_slot.unwrap_or(0);
    let end_slot = end_slot.unwrap_or(u64::MAX);
    let sorted = {
        let mut vec = ledger
            .iter_transaction_statuses(None, success)
            .filter_map(|res| match res {
                Ok((slot, sig, status))
                    if start_slot <= slot && slot <= end_slot =>
                {
                    if let Some(accounts) = &accounts {
                        // NOTE: I tried to use
                        // - status.loaded_writable_addresses
                        // - status.loaded_readonly_addresses
                        // but those are always empty
                        let tx = ledger
                            .get_complete_transaction(sig, u64::MAX)
                            .expect("Failed to get transaction");

                        let matching_keys = tx
                            .map(|x| {
                                x.get_transaction()
                                    .message
                                    .static_account_keys()
                                    .to_vec()
                            })
                            .unwrap_or_default()
                            .iter()
                            .filter(|pubkey| accounts.contains(pubkey))
                            .cloned()
                            .collect::<HashSet<_>>();

                        if !matching_keys.is_empty() {
                            Some((slot, sig, status, Some(matching_keys)))
                        } else {
                            None
                        }
                    } else {
                        Some((slot, sig, status, None))
                    }
                }
                Ok(_) => None,
                Err(_) => None,
            })
            .collect::<Vec<_>>();
        vec.sort_by_key(|(slot, _, _, _)| *slot);
        vec
    };
    for (slot, sig, status, acc) in sorted {
        println!("\n ------------------------------------");
        if let Some(x) = acc {
            println!(
                "\n## Matched Accounts: {}",
                x.iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        println!("\n### Transaction: {} ({})", sig, slot);
        println!("{}", render_logs(&status.log_messages, "  "));
    }
}
