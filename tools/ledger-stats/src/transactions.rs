use magicblock_ledger::Ledger;

pub(crate) fn print_transactions(ledger: &Ledger, success: bool) {
    let sorted = {
        let mut vec = ledger
            .iter_transaction_statuses(success)
            .filter_map(|res| match res {
                Ok(val) => Some(val),
                Err(_) => None,
            })
            .collect::<Vec<_>>();
        vec.sort_by_key(|(slot, _, _)| *slot);
        vec
    };
    for (slot, sig, status) in sorted {
        println!("\nTransaction: {} ({})", sig, slot);
        println!("  {}", status.log_messages.join("\n  "));
    }
}
