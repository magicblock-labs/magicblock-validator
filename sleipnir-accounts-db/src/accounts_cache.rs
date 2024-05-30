use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use dashmap::DashMap;
use solana_metrics::datapoint_info;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    pubkey::Pubkey,
};

// -----------------
// CachedAccount
// -----------------
pub type CachedAccount = Arc<CachedAccountInner>;
#[derive(Debug)]
pub struct CachedAccountInner {
    pub account: AccountSharedData,
    pubkey: Pubkey,
}

impl CachedAccountInner {
    pub fn pubkey(&self) -> &Pubkey {
        &self.pubkey
    }
}

// -----------------
// SlotCache
// -----------------
pub type SlotCache = Arc<SlotCacheInner>;

#[derive(Debug)]
pub struct SlotCacheInner {
    /// The underlying cache
    cache: DashMap<Pubkey, CachedAccount>,

    /// Tells us how many times we stored a specific account in the same slot
    same_account_writes: AtomicU64,
    /// The overall size of accounts that replaced an already existing account for a slot
    same_account_writes_size: AtomicU64,
    /// The overall size of accounts that were stored for the first time in a slot
    unique_account_writes_size: AtomicU64,

    /// Size of accounts currently stored in the cache
    size: AtomicU64,
    /// Size of accounts stored in the owning [AccountsCache] for all [SlotCache]s combined
    total_size: Arc<AtomicU64>,
}

impl Drop for SlotCacheInner {
    fn drop(&mut self) {
        // broader cache no longer holds our size in memory
        self.total_size
            .fetch_sub(self.size.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

impl SlotCacheInner {
    pub fn report_slot_store_metrics(&self) {
        datapoint_info!(
            "slot_repeated_writes",
            (
                "same_account_writes",
                self.same_account_writes.load(Ordering::Relaxed),
                i64
            ),
            (
                "same_account_writes_size",
                self.same_account_writes_size.load(Ordering::Relaxed),
                i64
            ),
            (
                "unique_account_writes_size",
                self.unique_account_writes_size.load(Ordering::Relaxed),
                i64
            ),
            ("size", self.size.load(Ordering::Relaxed), i64)
        );
    }

    pub fn get_all_pubkeys(&self) -> Vec<Pubkey> {
        self.cache.iter().map(|item| *item.key()).collect()
    }

    pub fn insert(
        &self,
        pubkey: &Pubkey,
        account: AccountSharedData,
    ) -> CachedAccount {
        let data_len = account.data().len() as u64;
        let item = Arc::new(CachedAccountInner {
            account,
            pubkey: *pubkey,
        });
        if let Some(old) = self.cache.insert(*pubkey, item.clone()) {
            // If we replace an entry in the same slot then we calculate the size differenc
            self.same_account_writes.fetch_add(1, Ordering::Relaxed);
            self.same_account_writes_size
                .fetch_add(data_len, Ordering::Relaxed);

            let old_len = old.account.data().len() as u64;
            let grow = data_len.saturating_sub(old_len);
            if grow > 0 {
                self.size.fetch_add(grow, Ordering::Relaxed);
                self.total_size.fetch_add(grow, Ordering::Relaxed);
            } else {
                let shrink = old_len.saturating_sub(data_len);
                if shrink > 0 {
                    self.size.fetch_sub(shrink, Ordering::Relaxed);
                    self.total_size.fetch_sub(shrink, Ordering::Relaxed);
                }
            }
        } else {
            // If we insert a new entry then we add its size
            self.size.fetch_add(data_len, Ordering::Relaxed);
            self.total_size.fetch_add(data_len, Ordering::Relaxed);
            self.unique_account_writes_size
                .fetch_add(data_len, Ordering::Relaxed);
        }

        item
    }

    pub fn get_cloned(&self, pubkey: &Pubkey) -> Option<CachedAccount> {
        self.cache
            .get(pubkey)
            .map(|account_ref| account_ref.value().clone())
    }

    pub fn total_bytes(&self) -> u64 {
        self.unique_account_writes_size.load(Ordering::Relaxed)
            + self.same_account_writes_size.load(Ordering::Relaxed)
    }
}

// -----------------
// AccountsCache
// -----------------
/// Caches account states for a particular slot.
#[derive(Debug, Default)]
pub struct AccountsCache {
    cache: DashMap<Slot, SlotCache>,
    total_size: Arc<AtomicU64>,
}

impl AccountsCache {
    pub fn new_inner(&self) -> SlotCache {
        Arc::new(SlotCacheInner {
            cache: DashMap::default(),
            same_account_writes: AtomicU64::default(),
            same_account_writes_size: AtomicU64::default(),
            unique_account_writes_size: AtomicU64::default(),
            size: AtomicU64::default(),
            total_size: Arc::clone(&self.total_size),
        })
    }

    pub fn size(&self) -> u64 {
        self.total_size.load(Ordering::Relaxed)
    }

    pub fn report_size(&self) {
        datapoint_info!(
            "accounts_cache_size",
            ("num_slots", self.cache.len(), i64),
            (
                "total_unique_writes_size",
                self.unique_account_writes_size(),
                i64
            ),
            ("total_size", self.size(), i64),
        );
    }

    fn unique_account_writes_size(&self) -> u64 {
        self.cache
            .iter()
            .map(|item| {
                let slot_cache = item.value();
                slot_cache
                    .unique_account_writes_size
                    .load(Ordering::Relaxed)
            })
            .sum()
    }

    pub fn store(
        &self,
        slot: Slot,
        pubkey: &Pubkey,
        account: AccountSharedData,
    ) -> CachedAccount {
        let slot_cache = self.slot_cache(slot).unwrap_or_else(||
            // DashMap entry.or_insert() returns a RefMut, essentially a write lock,
            // which is dropped after this block ends, minimizing time held by the lock.
            // Hence we clone it out, (`SlotStores` is an Arc so is cheap to clone).
            // However, we still want to persist the reference to the `SlotStores` behind
            self
                .cache
                .entry(slot)
                .or_insert(self.new_inner())
                .clone());

        slot_cache.insert(pubkey, account)
    }

    pub fn load(&self, slot: Slot, pubkey: &Pubkey) -> Option<CachedAccount> {
        self.slot_cache(slot)
            .and_then(|slot_cache| slot_cache.get_cloned(pubkey))
    }

    pub fn remove_slot(&self, slot: Slot) -> Option<SlotCache> {
        self.cache.remove(&slot).map(|(_, slot_cache)| slot_cache)
    }

    pub fn slot_cache(&self, slot: Slot) -> Option<SlotCache> {
        self.cache.get(&slot).map(|result| result.value().clone())
    }

    pub fn contains(&self, slot: Slot) -> bool {
        self.cache.contains_key(&slot)
    }

    pub fn num_slots(&self) -> usize {
        self.cache.len()
    }
}
