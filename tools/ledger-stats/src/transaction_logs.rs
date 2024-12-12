use crate::utils::render_logs;
use magicblock_ledger::Ledger;

pub(crate) fn print_transaction_logs(
    ledger: &Ledger,
    start_slot: Option<u64>,
    end_slot: Option<u64>,
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
                        Some((slot, sig, status))
                    }
                Ok(_) => None,
                Err(_) => None,
            })
            .collect::<Vec<_>>();
        vec.sort_by_key(|(slot, _, _)| *slot);
        vec
    };
    for (slot, sig, status) in sorted {
        println!("\nTransaction: {} ({})", sig, slot);
        println!("{}", render_logs(&status.log_messages, "  "));
    }
}
