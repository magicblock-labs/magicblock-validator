use std::{
    ffi::{CStr, CString},
    marker::PhantomData,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use log::trace;
use rocksdb::{
    compaction_filter::CompactionFilter,
    compaction_filter_factory::{
        CompactionFilterContext, CompactionFilterFactory,
    },
    CompactionDecision,
};
use solana_sdk::clock::Slot;

use crate::database::columns::{Column, ColumnName};

/// Factory that produces PurgedSlotFilter
/// This struct is used for deleting truncated slots from the DB during
/// RocksDB's scheduled compaction. The Factory creates the Filter.
/// We maintain oldest_slot that signals what slots were truncated and are safe to remove
pub(crate) struct PurgedSlotFilterFactory<C: Column + ColumnName> {
    oldest_slot: Arc<AtomicU64>,
    name: CString,
    _phantom: PhantomData<C>,
}

impl<C: Column + ColumnName> PurgedSlotFilterFactory<C> {
    pub fn new(oldest_slot: Arc<AtomicU64>) -> Self {
        let name = CString::new(format!(
            "purged_slot_filter({}, {:?})",
            C::NAME,
            oldest_slot
        ))
        .unwrap();
        Self {
            oldest_slot,
            name,
            _phantom: PhantomData,
        }
    }
}

impl<C: Column + ColumnName> CompactionFilterFactory
    for PurgedSlotFilterFactory<C>
{
    type Filter = PurgedSlotFilter<C>;

    fn create(&mut self, _context: CompactionFilterContext) -> Self::Filter {
        let copied_oldest_slot = self.oldest_slot.load(Ordering::Relaxed);
        let name = CString::new(format!(
            "purged_slot_filter({}, {:?})",
            C::NAME,
            copied_oldest_slot
        ))
        .unwrap();

        PurgedSlotFilter::<C> {
            oldest_slot: copied_oldest_slot,
            name,
            _phantom: PhantomData,
        }
    }

    fn name(&self) -> &CStr {
        &self.name
    }
}

/// A CompactionFilter implementation to remove keys older than a given slot.
pub(crate) struct PurgedSlotFilter<C: Column + ColumnName> {
    /// The oldest slot to keep; any slot < oldest_slot will be removed
    oldest_slot: Slot,
    name: CString,
    _phantom: PhantomData<C>,
}

impl<C: Column + ColumnName> CompactionFilter for PurgedSlotFilter<C> {
    fn filter(
        &mut self,
        level: u32,
        key: &[u8],
        _value: &[u8],
    ) -> CompactionDecision {
        use rocksdb::CompactionDecision::*;
        if C::keep_all_on_compaction() {
            return Keep;
        }
        trace!("CompactionFilter: triggered!");

        let slot_in_key = C::slot(C::index(key));
        if slot_in_key < self.oldest_slot {
            trace!(
                "CompactionFilter: removing key. level: {}, slot: {}",
                level,
                slot_in_key
            );

            // It is safe to delete this key
            // since those slots were truncated anyway
            Remove
        } else {
            Keep
        }
    }

    fn name(&self) -> &CStr {
        &self.name
    }
}
