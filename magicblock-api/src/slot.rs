use magicblock_bank::bank::Bank;
use magicblock_ledger::{errors::LedgerResult, Ledger};
use solana_sdk::clock::Slot;

pub fn advance_slot_and_update_ledger(
    bank: &Bank,
    ledger: &Ledger,
) -> (LedgerResult<()>, Slot) {
    let prev_slot = bank.slot();
    let prev_blockhash = bank.last_blockhash();

    // NOTE:
    // Each time we advance the slot, we check if a snapshot should be taken.
    // If the current slot is a multiple of the preconfigured snapshot frequency,
    // the AccountsDB will enforce a global lock before taking the snapshot. This
    // introduces a slight hiccup in transaction execution, which is an unavoidable
    // consequence of the need to flush in-memory data to disk, while ensuring no
    // writes occur during this operation. With small and CoW databases, this lock
    // should not exceed a few milliseconds.
    let next_slot = bank.advance_slot();

    // Update ledger with previous block's metas
    let ledger_result = ledger.write_block(
        prev_slot,
        bank.clock().unix_timestamp,
        prev_blockhash,
    );
    (ledger_result, next_slot)
}
