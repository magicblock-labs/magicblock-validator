use std::time::{SystemTime, UNIX_EPOCH};

use magicblock_bank::bank::Bank;
use magicblock_ledger::{errors::LedgerResult, Ledger};
use solana_sdk::clock::Slot;

pub fn advance_slot_and_update_ledger(
    bank: &Bank,
    ledger: &Ledger,
) -> (LedgerResult<()>, Slot) {
    let prev_slot = bank.slot();
    let prev_blockhash = bank.last_blockhash();

    // NOTE: this might introduce a slight hiccup in
    // case if the snapshot is to be taken at this slot
    let next_slot = bank.advance_slot();

    // Update ledger with previous block's metas
    let ledger_result = ledger.write_block(
        prev_slot,
        timestamp_in_secs() as i64,
        prev_blockhash,
    );
    (ledger_result, next_slot)
}

fn timestamp_in_secs() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("create timestamp in timing");
    now.as_secs()
}
