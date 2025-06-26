use async_trait::async_trait;
use log::*;
use solana_measure::measure::Measure;
use std::sync::Arc;
use tokio::task::JoinSet;

use solana_sdk::clock::Slot;

use crate::{
    database::columns::{
        AddressSignatures, Blockhash, Blocktime, PerfSamples, SlotSignatures,
        Transaction, TransactionMemos, TransactionStatus,
    },
    errors::{LedgerError, LedgerResult},
    Ledger,
};

use super::traits::ManagableLedger;

pub struct Truncator {
    ledger: Arc<Ledger>,
}

impl Truncator {
    /// Synchronous utility function that triggers and awaits compaction on all the columns
    /// Compacts [from_slot; to_slot] inclusive
    pub async fn compact_slot_range(
        ledger: &Arc<Ledger>,
        from_slot: u64,
        to_slot: u64,
    ) {
        debug_assert!(to_slot >= from_slot, "to_slot >= from_slot");
        if to_slot < from_slot {
            error!("BUG: to_slot < from_slot, not compacting");
            return;
        }

        // Compaction can be run concurrently for different cf
        // but it utilizes rocksdb threads, in order not to drain
        // our tokio rt threads, we split the effort in just 3 tasks
        let mut measure = Measure::start("Manual compaction");
        let mut join_set = JoinSet::new();
        join_set.spawn_blocking({
            let ledger = ledger.clone();
            move || {
                ledger.compact_slot_range_cf::<Blocktime>(
                    Some(from_slot),
                    Some(to_slot + 1),
                );
                ledger.compact_slot_range_cf::<Blockhash>(
                    Some(from_slot),
                    Some(to_slot + 1),
                );
                ledger.compact_slot_range_cf::<PerfSamples>(
                    Some(from_slot),
                    Some(to_slot + 1),
                );
                ledger.compact_slot_range_cf::<SlotSignatures>(
                    Some((from_slot, u32::MIN)),
                    Some((to_slot + 1, u32::MAX)),
                );
            }
        });

        // The below we cannot compact with specific range since they keys don't
        // start with the slot value
        join_set.spawn_blocking({
            let ledger = ledger.clone();
            move || {
                ledger.compact_slot_range_cf::<TransactionStatus>(None, None);
                ledger.compact_slot_range_cf::<Transaction>(None, None);
            }
        });
        join_set.spawn_blocking({
            let ledger = ledger.clone();
            move || {
                ledger.compact_slot_range_cf::<TransactionMemos>(None, None);
                ledger.compact_slot_range_cf::<AddressSignatures>(None, None);
            }
        });

        join_set.join_all().await;
        measure.stop();
        debug!("Manual compaction took: {measure}");
    }
}

#[async_trait]
impl ManagableLedger for Truncator {
    fn storage_size(&self) -> Result<u64, LedgerError> {
        self.ledger.storage_size()
    }

    fn last_slot(&self) -> Slot {
        self.ledger.last_slot()
    }

    fn last_mod_id(&self) -> u64 {
        self.ledger.last_mod_id()
    }

    fn initialize_lowest_cleanup_slot(&self) -> Result<(), LedgerError> {
        self.ledger.initialize_lowest_cleanup_slot()
    }

    async fn compact_slot_range(&self, from: Slot, to: Slot) {
        Self::compact_slot_range(&self.ledger, from, to).await;
    }

    async fn truncate_fat_ledger(&self, lowest_slot: u64) {
        self.ledger.set_lowest_cleanup_slot(lowest_slot);
        if let Err(err) = self.ledger.flush() {
            // We will still compact
            error!("Failed to flush: {}", err);
        }
        Self::compact_slot_range(&self.ledger, 0, lowest_slot).await;
    }
}
