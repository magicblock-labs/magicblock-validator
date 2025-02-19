use std::time::{SystemTime, UNIX_EPOCH};

use magicblock_bank::bank::Bank;
use magicblock_ledger::{errors::LedgerResult, Ledger};
use magicblock_processor::execute_transaction::lock_transactions;
use solana_sdk::clock::Slot;

pub fn advance_slot_and_update_ledger(
    bank: &Bank,
    ledger: &Ledger,
) -> (LedgerResult<()>, Slot) {
    let prev_slot = bank.slot();
    let prev_blockhash = bank.last_blockhash();

    let next_slot = if prev_slot % FLUSH_ACCOUNTS_SLOT_FREQ == 0 {
        // NOTE: at this point we flush the accounts blocking the slot from advancing as
        // well as holding the transaction lock.
        // This is done on purpose in order to avoid transactions writing to the accounts
        // while we are persisting them.
        // This is a very slow operation, i.e. in the 30ms+ range and we should consider
        // making a copy of all accounts, including data and then performing the IO flush
        // in a separate task.
        // Also in this case we prevent the transactions from advancing before the bank
        // slot advanced since only then can we be sure that the accounts did not change
        // during the same slot after we flushed them.
        let _lock = lock_transactions();
        bank.advance_slot()
    } else {
        bank.advance_slot()
    };

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
