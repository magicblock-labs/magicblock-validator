use std::sync::atomic::{AtomicU64, Ordering};

use solana_measure::measure::Measure;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    genesis_config::ClusterType,
    pubkey::Pubkey,
    transaction::SanitizedTransaction,
};

use crate::{
    account_info::{AccountInfo, StorageLocation},
    accounts_cache::{AccountsCache, CachedAccount},
    accounts_index::ZeroLamport,
    accounts_update_notifier_interface::AccountsUpdateNotifier,
    errors::MatchAccountOwnerError,
    storable_accounts::StorableAccounts,
};

mod loaded_account_accessor;
pub use loaded_account_accessor::LoadedAccountAccessor;

pub type StoredMetaWriteVersion = u64;

// -----------------
// StoreTo
// -----------------
#[derive(Debug)]
enum StoreTo {
    /// write to cache
    Cache,
    // NOTE: not yet supporting write to storage
}

impl StoreTo {
    fn is_cached(&self) -> bool {
        matches!(self, StoreTo::Cache)
    }
}

// -----------------
// AccountStats
// -----------------
#[derive(Debug, Default)]
pub struct AccountsStats {
    store_num_accounts: AtomicU64,
    store_total_data: AtomicU64,
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

    pub cluster_type: Option<ClusterType>,
}

impl AccountsDb {
    pub fn new_with_config(
        cluster_type: &ClusterType,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
    ) -> Self {
        Self {
            cluster_type: Some(*cluster_type),
            accounts_cache: AccountsCache::default(),
            stats: AccountsStats::default(),
            accounts_update_notifier,
            write_version: AtomicU64::default(),
        }
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
        store_to: &StoreTo,
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
        store_to: &StoreTo,
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

        // TODO(thlorenz): @@@ Reclaim Logic in order to remove no longer needed accounts
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
        store_to: &StoreTo,
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

                self.accounts_cache.store(
                    slot,
                    accounts_and_meta_to_store.pubkey(i),
                    account,
                );
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
        let (slot, storage_location, cached_account) = self
            .read_index_for_accessor(account)
            .ok_or(MatchAccountOwnerError::UnableToLoad)?;

        debug_assert!(
            storage_location.is_cached(),
            "We only store in the cache"
        );

        if cached_account.is_zero_lamport() {
            None
        } else {
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
