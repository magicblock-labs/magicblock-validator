use rayon::{prelude::*, ThreadPool};
use solana_measure::measure::Measure;
use solana_rayon_threadlimit::get_thread_count;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    genesis_config::ClusterType,
    pubkey::Pubkey,
    transaction::SanitizedTransaction,
    transaction_context::TransactionAccount,
};
use stats::FlushStats;
use std::sync::Arc;
use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
};

use crate::{
    account_info::{AccountInfo, StorageLocation},
    accounts_cache::{AccountsCache, CachedAccount, SlotCache},
    accounts_index::ZeroLamport,
    accounts_update_notifier_interface::AccountsUpdateNotifier,
    errors::MatchAccountOwnerError,
    persist::AccountsPersister,
    storable_accounts::StorableAccounts,
    verify_accounts_hash_in_background::VerifyAccountsHashInBackground,
};

mod consts;
mod loaded_account;
mod loaded_account_accessor;
mod stats;
use self::{
    consts::SCAN_SLOT_PAR_ITER_THRESHOLD, loaded_account::LoadedAccount,
};
use crate::account_storage::AccountStorageEntry;
pub use loaded_account_accessor::LoadedAccountAccessor;

pub type StoredMetaWriteVersion = u64;

// -----------------
// StoreTo
// -----------------
#[derive(Debug)]
pub enum StoreTo<'a> {
    /// write to cache
    Cache,
    /// write to storage
    Storage(&'a Arc<AccountStorageEntry>),
}

// -----------------
// ScanStorageResult
// -----------------
pub enum ScanStorageResult<R> {
    Cached(Vec<R>),
    // NOTE: not yet supporting Store
}

// -----------------
// AccountStats
// -----------------
#[derive(Debug, Default)]
pub struct AccountsStats {
    store_num_accounts: AtomicU64,
    store_accounts: AtomicU64,

    // NOTE: we don't support staking but kept the name for now
    pub stakes_cache_check_and_store_us: AtomicU64,
}

// -----------------
// AccountsDb
// -----------------
// This structure handles the load/store of the accounts
#[derive(Debug)]
pub struct AccountsDb {
    /// The cache of accounts which is the only storage we use at this point
    pub accounts_cache: AccountsCache,

    /// Stats about account stores
    pub stats: AccountsStats,

    /// GeyserPlugin accounts update notifier
    accounts_update_notifier: Option<AccountsUpdateNotifier>,

    /// Write version used to notify accounts in order to distinguish between
    /// multiple updates to the same account in the same slot
    pub write_version: AtomicU64,

    persister: AccountsPersister,

    pub cluster_type: Option<ClusterType>,

    /// Thread pool used for par_iter
    pub thread_pool: ThreadPool,

    pub verify_accounts_hash_in_bg: VerifyAccountsHashInBackground,
}

impl AccountsDb {
    pub fn default_for_tests() -> Self {
        Self::new(None, None, AccountsPersister::default())
    }

    pub fn new_with_config(
        cluster_type: &ClusterType,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
    ) -> Self {
        // TODO(thlorenz): @@@ provide paths
        let accounts_persister = AccountsPersister::default();
        Self::new(
            Some(*cluster_type),
            accounts_update_notifier,
            accounts_persister,
        )
    }

    pub fn new(
        cluster_type: Option<ClusterType>,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        persister: AccountsPersister,
    ) -> Self {
        let num_threads = get_thread_count();
        // rayon needs a lot of stack
        const ACCOUNTS_STACK_SIZE: usize = 8 * 1024 * 1024;
        Self {
            cluster_type,
            accounts_cache: AccountsCache::default(),
            stats: AccountsStats::default(),
            accounts_update_notifier,
            write_version: AtomicU64::default(),
            persister,
            thread_pool: rayon::ThreadPoolBuilder::new()
                .num_threads(num_threads)
                .thread_name(|i| format!("solAccounts{i:02}"))
                .stack_size(ACCOUNTS_STACK_SIZE)
                .build()
                .unwrap(),
            verify_accounts_hash_in_bg: VerifyAccountsHashInBackground::default(
            ),
        }
    }

    pub fn set_slot(&self, slot: Slot) {
        self.accounts_cache.set_current_slot(slot);
    }

    // -----------------
    // Store Operations
    // -----------------
    pub fn store_cached<'a, T: ReadableAccount + Sync + ZeroLamport + 'a>(
        &self,
        accounts: impl StorableAccounts<'a, T>,
        transactions: Option<&'a [Option<&'a SanitizedTransaction>]>,
    ) {
        self.store(accounts, &StoreTo::Cache, transactions);
    }

    fn store<'a, T: ReadableAccount + Sync + ZeroLamport + 'a>(
        &self,
        accounts: impl StorableAccounts<'a, T>,
        store_to: &'a StoreTo,
        transactions: Option<&'a [Option<&'a SanitizedTransaction>]>,
        // NOTE: we don't take an UpdateIndexThreadSelection strategy here since we
        // always store in the cache at this point
    ) {
        // If all transactions in a batch are errored,
        // it's possible to get a store with no accounts.
        if accounts.is_empty() {
            return;
        }

        // NOTE: not hashing, so we don't record bank_hash_stats either

        // NOTE: skipping the store_accounts_unfrozen redirection since we
        // always store into unfrozen (current) slot

        // TODO: @@@ Connect this to flush flow
        self.store_accounts_custom(
            accounts,
            None::<Box<dyn Iterator<Item = u64>>>,
            store_to,
            transactions,
        );
    }

    fn store_accounts_custom<
        'a,
        T: ReadableAccount + Sync + ZeroLamport + 'a,
    >(
        &self,
        accounts: impl StorableAccounts<'a, T>,
        // This is `None` for cached accounts
        write_version_producer: Option<Box<dyn Iterator<Item = u64>>>,
        store_to: &'a StoreTo,
        transactions: Option<&'a [Option<&'a SanitizedTransaction>]>,
    ) {
        let write_version_producer: Box<dyn Iterator<Item = u64>> =
            write_version_producer.unwrap_or_else(|| {
                let mut current_version =
                    self.bulk_assign_write_version(accounts.len());
                Box::new(std::iter::from_fn(move || {
                    let ret = current_version;
                    current_version += 1;
                    Some(ret)
                }))
            });
        // NOTE: non-frozen stores don't have a write_version_producer so we skip
        // related logic entirely

        self.stats
            .store_num_accounts
            .fetch_add(accounts.len() as u64, Ordering::Relaxed);

        let mut store_accounts_time = Measure::start("store_accounts");
        let _infos = self.store_accounts_to(
            &accounts,
            write_version_producer,
            store_to,
            transactions,
        );
        store_accounts_time.stop();
        self.stats
            .store_accounts
            .fetch_add(store_accounts_time.as_us(), Ordering::Relaxed);

        // NOTE: left out reclaim logic that removes no longer needed accounts
        // since we only keep one copy of each account
    }

    fn store_accounts_to<
        'a: 'c,
        'b,
        'c,
        P: Iterator<Item = u64>,
        T: ReadableAccount + Sync + ZeroLamport + 'b,
    >(
        &self,
        accounts: &'c impl StorableAccounts<'b, T>,
        mut write_version_producer: P,
        store_to: &'b StoreTo,
        transactions: Option<&[Option<&'a SanitizedTransaction>]>,
    ) -> Vec<AccountInfo> {
        // NOTE: left out 'calc_stored_meta' which removed accounts from readonly cache

        let slot = accounts.target_slot();
        match store_to {
            StoreTo::Cache => {
                let txn_iter: Box<
                    dyn std::iter::Iterator<
                        Item = &Option<&SanitizedTransaction>,
                    >,
                > = match transactions {
                    Some(transactions) => {
                        assert_eq!(transactions.len(), accounts.len());
                        Box::new(transactions.iter())
                    }
                    None => {
                        Box::new(std::iter::repeat(&None).take(accounts.len()))
                    }
                };

                self.write_accounts_to_cache(
                    slot,
                    accounts,
                    txn_iter,
                    &mut write_version_producer,
                )
            }
            // TODO: @@@ Decide where this entry is actually created
            // the persister know how to do it, but it seems like it should be provided
            // here as input already.
            StoreTo::Storage(_entry) => todo!("store to storage"),
            /*
                        self.persister.store_accounts(
                            accounts,
                            None::<Vec<AccountHash>>,
                            write_version_producer,
                            slot,
                        ),
            */
        }
    }

    fn write_accounts_to_cache<'a, 'b, T: ReadableAccount + Sync, P>(
        &self,
        slot: Slot,
        accounts_and_meta_to_store: &impl StorableAccounts<'b, T>,
        txn_iter: Box<
            dyn std::iter::Iterator<Item = &Option<&SanitizedTransaction>> + 'a,
        >,
        write_version_producer: &mut P,
    ) -> Vec<AccountInfo>
    where
        P: Iterator<Item = u64>,
    {
        txn_iter
            .enumerate()
            .map(|(i, txn)| {
                let account = accounts_and_meta_to_store
                    .account_default_if_zero_lamport(i)
                    .map(|account| account.to_account_shared_data())
                    .unwrap_or_default();
                let account_info = AccountInfo::new(
                    StorageLocation::Cached,
                    account.lamports(),
                );

                self.notify_account_at_accounts_update(
                    slot,
                    &account,
                    txn,
                    accounts_and_meta_to_store.pubkey(i),
                    write_version_producer,
                );

                self.accounts_cache
                    .store(accounts_and_meta_to_store.pubkey(i), account);
                // NOTE: not sending hash request to sender_bg_hasher
                account_info
            })
            .collect()
    }

    /// Increases [Self::write_version] by `count` and returns the previous value
    fn bulk_assign_write_version(
        &self,
        count: usize,
    ) -> StoredMetaWriteVersion {
        self.write_version
            .fetch_add(count as StoredMetaWriteVersion, Ordering::AcqRel)
    }

    // -----------------
    // Query Operations
    // -----------------
    /// Return Ok(index_of_matching_owner) if the account owner at `offset` is one of the pubkeys in `owners`.
    /// Return Err(MatchAccountOwnerError::NoMatch) if the account has 0 lamports or the owner is not one of
    /// the pubkeys in `owners`.
    /// Return Err(MatchAccountOwnerError::UnableToLoad) if the account could not be accessed.
    // NOTE: this is called from sleipnir-bank/src/bank.rs fn account_matches_owners and
    // it is confusing why the original implementation is so complex if we just return an
    // index into the already provided [owners] array
    pub fn account_matches_owners(
        &self,
        account: &Pubkey,
        owners: &[Pubkey],
    ) -> Result<usize, MatchAccountOwnerError> {
        // 1. Check if the account is stored
        let (_slot, storage_location, cached_account) = self
            .read_index_for_accessor(account)
            .ok_or(MatchAccountOwnerError::UnableToLoad)?;

        debug_assert!(
            storage_location.is_cached(),
            "We only store in the cache"
        );

        // 2. Ensure that it still _really_ exists
        if cached_account.is_zero_lamport() {
            None
        } else {
            // 3. And that one of the owner matches
            owners
                .iter()
                .position(|entry| cached_account.account.owner() == entry)
        }
        .ok_or(MatchAccountOwnerError::NoMatch)
    }

    pub fn load(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.accounts_cache
            .load(pubkey)
            .map(|cached_account| cached_account.account.clone())
    }

    pub fn load_with_slot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.accounts_cache
            .load_with_slot(pubkey)
            .map(|(account, slot)| (account.account.clone(), slot))
    }

    // NOTE: the original implementation was called read_index_for_accessor_or_load_slow and did
    // optionally return LoadedAccountAccessor.
    fn read_index_for_accessor(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(Slot, StorageLocation, CachedAccount)> {
        let (cached_account, slot) =
            self.accounts_cache.load_with_slot(pubkey)?;

        // If we add a storage location we need to obtain the AccountInfo
        // The original implementation get this from from the slot_list
        let storage_location = StorageLocation::Cached;

        // NOTE: left out the `load_slow` logic since we only store in the cache
        Some((slot, storage_location, cached_account))
    }

    pub fn scan_account_storage<R>(
        &self,
        cache_map_func: impl Fn(LoadedAccount) -> Option<R> + Sync,
    ) -> ScanStorageResult<R>
    where
        R: Send,
    {
        if self.accounts_cache.len() > SCAN_SLOT_PAR_ITER_THRESHOLD {
            ScanStorageResult::Cached(self.thread_pool.install(|| {
                self.accounts_cache
                    .slot_cache()
                    .par_iter()
                    .filter_map(|cached_account| {
                        cache_map_func(LoadedAccount::Cached(Cow::Borrowed(
                            cached_account.value(),
                        )))
                    })
                    .collect()
            }))
        } else {
            ScanStorageResult::Cached(
                self.accounts_cache
                    .slot_cache()
                    .iter()
                    .filter_map(|cached_account| {
                        cache_map_func(LoadedAccount::Cached(Cow::Borrowed(
                            cached_account.value(),
                        )))
                    })
                    .collect(),
            )
        }
    }

    pub fn scan_accounts(
        &self,
        scan_func: impl Fn(&Pubkey, AccountSharedData) -> bool + Send + Sync,
        config: &solana_accounts_db::accounts_index::ScanConfig,
    ) -> Vec<TransactionAccount> {
        // NOTE: here we differ a lot from the original Solana implementation which
        // scans the account index, tries to load the account and invokes
        // the scan_func with the account an Option<(&Pubkey, AccountSharedData, Slot)>

        let mut accounts =
            if self.accounts_cache.len() > SCAN_SLOT_PAR_ITER_THRESHOLD {
                let collected = RwLock::<Vec<TransactionAccount>>::default();
                self.thread_pool.install(|| {
                    self.accounts_cache
                        .slot_cache()
                        .par_iter()
                        .filter_map(|cached_account| {
                            let pubkey = *cached_account.pubkey();
                            let account = &cached_account.value().account;
                            if scan_func(&pubkey, account.clone()) {
                                Some((pubkey, account.clone()))
                            } else {
                                None
                            }
                        })
                        .for_each(|(pubkey, account)| {
                            collected.write().unwrap().push((pubkey, account))
                        });
                });
                collected.into_inner().unwrap()
            } else {
                self.accounts_cache
                    .slot_cache()
                    .iter()
                    .filter_map(|cached_account| {
                        let pubkey = *cached_account.pubkey();
                        let account = &cached_account.value().account;
                        if scan_func(&pubkey, account.clone()) {
                            Some((pubkey, account.clone()))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            };
        if config.collect_all_unsorted {
            accounts
        } else {
            accounts.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            accounts
        }
    }

    // -----------------
    // Persistence
    // -----------------
    // `force_flush` flushes all the cached roots `<= requested_flush_root`. It also then
    // flushes:
    // 1) excess remaining roots or unrooted slots while 'should_aggressively_flush_cache' is true
    // @@@ Accounts needs to be called by bank
    #[allow(unused)]
    pub fn flush_accounts_cache(
        &self,
        force_flush: bool,
        requested_flush_root: Option<Slot>,
    ) {
        let mut account_bytes_saved = 0;
        let mut num_accounts_saved = 0;

        let (
            total_new_cleaned_roots,
            num_cleaned_roots_flushed,
            mut flush_stats,
        ) = self.flush_rooted_accounts_cache(
            requested_flush_root,
            Some((&mut account_bytes_saved, &mut num_accounts_saved)),
        );

        todo!()
    }

    #[allow(unused)]
    fn flush_rooted_accounts_cache(
        &self,
        requested_flush_root: Option<Slot>,
        should_clean: Option<(&mut usize, &mut usize)>,
    ) -> (usize, usize, FlushStats) {
        //       let mut written_accounts = HashSet::new();
        todo!()
    }

    fn do_flush_slot_cache(&self, _slot: Slot, slot_cache: SlotCache) {
        let iter_items: Vec<_> = slot_cache.iter().collect();
        let accounts: Vec<(&Pubkey, &AccountSharedData)> = iter_items
            .iter()
            .map(|iter_item| {
                let key = iter_item.key();
                let account = &iter_item.value().account;
                // flush_stats.total_size +=
                //     aligned_stored_size(account.data().len()) as u64;
                // flush_stats.num_flushed += 1;

                (key, account)
            })
            .collect();

        let _is_dead_slot = accounts.is_empty();

        // TODO: @@ self.purge_slot_cache_pubkeys

        /*
        if !is_dead_slot {
            // This ensures that all updates are written to an AppendVec, before any
            // updates to the index happen, so anybody that sees a real entry in the index,
            // will be able to find the account in storage
            let flushed_store = self.create_and_insert_store(
                slot,
                flush_stats.total_size.0,
                "flush_slot_cache",
            );
            let (store_accounts_timing_inner, store_accounts_total_inner_us) =
                measure_us!(self.store_accounts_frozen(
                    (slot, &accounts[..]),
                    &flushed_store,
                ));
            flush_stats.store_accounts_timing = store_accounts_timing_inner;
            flush_stats.store_accounts_total_us =
                Saturating(store_accounts_total_inner_us);

            // If the above sizing function is correct, just one AppendVec is enough to hold
            // all the data for the slot
            assert!(self.storage.get_slot_storage_entry(slot).is_some());
            self.reopen_storage_as_readonly_shrinking_in_progress_ok(slot);
        }
        */
    }
    // -----------------
    // Geyser
    // -----------------
    pub fn notify_account_at_accounts_update<P>(
        &self,
        slot: Slot,
        account: &AccountSharedData,
        txn: &Option<&SanitizedTransaction>,
        pubkey: &Pubkey,
        write_version_producer: &mut P,
    ) where
        P: Iterator<Item = u64>,
    {
        if let Some(accounts_update_notifier) = &self.accounts_update_notifier {
            accounts_update_notifier.notify_account_update(
                slot,
                account,
                txn,
                pubkey,
                write_version_producer.next().unwrap(),
            );
        }
    }
}
