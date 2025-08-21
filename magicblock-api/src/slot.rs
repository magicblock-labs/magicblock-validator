use std::time::{SystemTime, UNIX_EPOCH};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::blocks::{BlockMeta, BlockUpdate, BlockUpdateTx};
use magicblock_ledger::{errors::LedgerResult, Ledger};
use solana_sdk::clock::Slot;
use solana_sdk::hash::Hasher;

pub fn advance_slot_and_update_ledger(
    accountsdb: &AccountsDb,
    ledger: &Ledger,
    block_update_tx: &BlockUpdateTx,
) -> (LedgerResult<()>, Slot) {
    // This is the latest "confirmed" block, written to the ledger
    let latest_block = ledger.latest_block().load();
    // And this is not yet "confirmed" slot, which doesn't have an associated "block"
    // same as latest_block.slot + 1, accountsdb is always 1 slot ahead of the ledger;
    let current_slot = accountsdb.slot();
    // Determine next blockhash
    let blockhash = {
        // In the Solana implementation there is a lot of logic going on to determine the next
        // blockhash, however we don't really produce any blocks, so any new hash will do.
        // Therefore we derive it from the previous hash and the current slot.
        let mut hasher = Hasher::default();
        hasher.hash(latest_block.blockhash.as_ref());
        hasher.hash(&current_slot.to_le_bytes());
        hasher.result()
    };

    // current slot is "finalized", and next slot becomes active
    let next_slot = current_slot + 1;
    // NOTE:
    // Each time we advance the slot, we check if a snapshot should be taken.
    // If the current slot is a multiple of the preconfigured snapshot frequency,
    // the AccountsDB will enforce a global lock before taking the snapshot. This
    // introduces a slight hiccup in transaction execution, which is an unavoidable
    // consequence of the need to flush in-memory data to disk, while ensuring no
    // writes occur during this operation. With small and CoW databases, this lock
    // should not exceed a few milliseconds.
    accountsdb.set_slot(next_slot);

    // As we have a single node network, we have no option but to use the time from host machine
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        // NOTE: since we can tick very frequently, a lot of blocks might have identical timestamps
        .as_secs() as i64;
    // Update ledger with previous block's meta, this will also notify various
    // listeners (like transaction executors) that block has been "produced"
    let ledger_result = ledger.write_block(current_slot, timestamp, blockhash);
    // also notify downstream subscribers (RPC/Geyser) that block has been produced
    let update = BlockUpdate {
        hash: blockhash,
        meta: BlockMeta {
            slot: current_slot,
            time: timestamp,
        },
    };
    let _ = block_update_tx.send(update);

    (ledger_result, next_slot)
}
