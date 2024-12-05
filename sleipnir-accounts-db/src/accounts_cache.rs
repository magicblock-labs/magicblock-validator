use std::{
    ops::{Deref, Neg},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
};

use dashmap::DashMap;
use solana_metrics::datapoint_info;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    pubkey::Pubkey,
};

use crate::{
    accounts_hash::AccountHash, accounts_index::ZeroLamport,
    persist::hash_account,
};

// -----------------
// CachedAccount
// -----------------
pub type CachedAccount = Arc<CachedAccountInner>;
#[derive(Debug)]
pub struct CachedAccountInner {
    pub account: AccountSharedData,
    // NOTE: solana-accountsdb uses a seqlock::SeqLock here which claims some perf improvements
    // over RwLock. See: https://github.com/Amanieu/seqlock
    hash: RwLock<Option<AccountHash>>,
    pubkey: Pubkey,
}

impl CachedAccountInner {
    /// Constructor method, in order to maintain memory
    /// consumption related metrics, the type should be
    /// constructed only through this constructor
    pub fn new(account: AccountSharedData, pubkey: Pubkey) -> Arc<Self> {
        Self {
            account,
            // NOTE: this Arc is needed in order to return the `CachedAccount` so that
            // its hash can be computed in the background once and then reused later
            hash: RwLock::<Option<AccountHash>>::default(),
            pubkey,
        }
        .into()
    }

    pub fn hash(&self) -> AccountHash {
        let hash = *self.hash.read().expect("hash lock poisoned");
        match hash {
            Some(hash) => hash,
            None => {
                let hash = hash_account(&self.account, &self.pubkey);
                *self.hash.write().expect("hash lock poisoned") = Some(hash);
                hash
            }
        }
    }
    pub fn pubkey(&self) -> &Pubkey {
        &self.pubkey
    }

    fn size(&self) -> i64 {
        const STATIC: i64 = size_of::<CachedAccountInner>() as i64;
        let dynamic = self.account.data().len() as i64;
        STATIC + dynamic
    }
}

// Custom Drop implementation allows us to forge about
// where deallocations of type happen, leaving cleanup,
// like metrics update, to compiler instead
impl Drop for CachedAccountInner {
    fn drop(&mut self) {
        let delta = self.size().neg();
        magicblock_metrics::metrics::adjust_inmemory_accounts_size(delta);
    }
}

impl ZeroLamport for CachedAccountInner {
    fn is_zero_lamport(&self) -> bool {
        self.account.lamports() == 0
    }
}

// -----------------
// SlotCache
// -----------------
pub type SlotCache = Arc<SlotCacheInner>;

#[derive(Debug, Default)]
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
}

impl Deref for SlotCacheInner {
    type Target = DashMap<Pubkey, CachedAccount>;
    fn deref(&self) -> &Self::Target {
        &self.cache
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
        let item = CachedAccountInner::new(account, *pubkey);
        if let Some(old) = self.cache.insert(*pubkey, item.clone()) {
            // If we replace an entry in the same slot then we calculate the size differenc
            self.same_account_writes.fetch_add(1, Ordering::Relaxed);
            self.same_account_writes_size
                .fetch_add(data_len, Ordering::Relaxed);

            let old_len = old.account.data().len() as u64;
            let grow = data_len.saturating_sub(old_len);
            if grow > 0 {
                self.size.fetch_add(grow, Ordering::Relaxed);
            } else {
                let shrink = old_len.saturating_sub(data_len);
                if shrink > 0 {
                    self.size.fetch_sub(shrink, Ordering::Relaxed);
                }
            }
        } else {
            // If we insert a new entry then we add its size
            self.size.fetch_add(data_len, Ordering::Relaxed);
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

    fn contains_key(&self, pubkey: &Pubkey) -> bool {
        self.cache.contains_key(pubkey)
    }
}

// -----------------
// AccountsCache
// -----------------
/// Caches account states for the current slot.
#[derive(Debug, Default)]
pub struct AccountsCache {
    slot_cache: SlotCache,
    current_slot: AtomicU64,
}

impl Deref for AccountsCache {
    type Target = SlotCache;

    fn deref(&self) -> &Self::Target {
        &self.slot_cache
    }
}

impl AccountsCache {
    pub fn new_inner(&self) -> SlotCache {
        Arc::new(SlotCacheInner {
            cache: DashMap::default(),
            same_account_writes: AtomicU64::default(),
            same_account_writes_size: AtomicU64::default(),
            unique_account_writes_size: AtomicU64::default(),
            size: AtomicU64::default(),
        })
    }

    pub fn size(&self) -> u64 {
        self.size.load(Ordering::Relaxed)
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
        self.unique_account_writes_size.load(Ordering::Relaxed)
    }

    pub fn store(
        &self,
        pubkey: &Pubkey,
        account: AccountSharedData,
    ) -> CachedAccount {
        self.slot_cache.insert(pubkey, account)
    }

    pub fn load(&self, pubkey: &Pubkey) -> Option<CachedAccount> {
        self.slot_cache.get_cloned(pubkey)
    }

    pub fn load_verifying_slot(
        &self,
        slot: Slot,
        pubkey: &Pubkey,
    ) -> Option<CachedAccount> {
        assert_eq!(
            slot,
            self.current_slot(),
            "we only allow loading accounts from current slot"
        );
        self.slot_cache.get_cloned(pubkey)
    }

    pub fn load_with_slot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(CachedAccount, Slot)> {
        self.slot_cache
            .get_cloned(pubkey)
            .map(|account| (account, self.current_slot()))
    }

    pub fn contains_key(&self, pubkey: &Pubkey) -> bool {
        self.slot_cache.contains_key(pubkey)
    }

    pub fn slot_cache(&self) -> SlotCache {
        self.slot_cache.clone()
    }

    pub fn num_slots(&self) -> usize {
        1
    }

    pub fn current_slot(&self) -> Slot {
        self.current_slot.load(Ordering::Relaxed)
    }

    pub fn set_current_slot(&self, slot: Slot) {
        self.current_slot.store(slot, Ordering::Relaxed);
    }

    pub fn len(&self) -> usize {
        self.slot_cache.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
