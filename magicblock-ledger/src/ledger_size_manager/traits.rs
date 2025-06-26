use async_trait::async_trait;
use solana_sdk::clock::Slot;

use crate::errors::{LedgerError, LedgerResult};

#[async_trait]
pub trait ManagableLedger: Send + Sync + 'static {
    fn storage_size(&self) -> LedgerResult<u64>;
    fn last_slot(&self) -> Slot;
    fn last_mod_id(&self) -> u64;
    fn initialize_lowest_cleanup_slot(&self) -> LedgerResult<()>;
    async fn compact_slot_range(&self, from: Slot, to: Slot);
    async fn truncate_fat_ledger(&self, lowest_slot: u64);
}
