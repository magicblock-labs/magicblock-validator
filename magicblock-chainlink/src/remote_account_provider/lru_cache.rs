use std::{collections::HashSet, num::NonZeroUsize};

use lru::LruCache;
use magicblock_metrics::metrics::inc_evicted_accounts_count;
use parking_lot::Mutex;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar;
use tracing::*;

use crate::submux::SubscribedAccountsTracker;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddAccountOutcome {
    AlreadyPresent,
    Added,
    Evicted(Pubkey),
    NoEvictableCandidate,
}

/// A wrapper around [lru::LruCache] for live account subscriptions.
///
/// The LRU is bookkeeping for subscribed accounts and may transiently contain
/// delegated or undelegating entries. Capacity eviction must skip entries that
/// are protected by account state or subscription ownership instead of blindly
/// using the raw [lru::LruCache] eviction result.
pub struct AccountsLruCache {
    /// Tracks which accounts are currently subscribed to; entries may include
    /// delegated or undelegating accounts until protected eviction filtering or
    /// explicit subscription cleanup removes them.
    subscribed_accounts: Mutex<LruCache<Pubkey, ()>>,
    accounts_to_never_evict: HashSet<Pubkey>,
}

fn accounts_to_never_evict() -> HashSet<Pubkey> {
    let mut set = HashSet::new();
    set.insert(sysvar::clock::id());
    set
}

impl AccountsLruCache {
    pub fn new(capacity: NonZeroUsize) -> Self {
        let accounts_to_never_evict = accounts_to_never_evict();
        Self {
            // SAFETY: NonZeroUsize::new only returns None if the value is 0.
            // RemoteAccountProviderConfig can only be constructed with
            // capacity > 0 thus the capacity here is guaranteed to be non-zero.
            subscribed_accounts: Mutex::new(LruCache::new(capacity)),
            accounts_to_never_evict,
        }
    }

    pub fn promote_multi(&self, pubkeys: &[&Pubkey]) {
        if tracing::enabled!(tracing::Level::TRACE) {
            let pubkeys = pubkeys
                .iter()
                .map(|pk| pk.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trace!(pubkeys = pubkeys, "Promoting accounts");
        }

        let mut subs = self.subscribed_accounts.lock();
        for key in pubkeys {
            subs.promote(key);
        }
    }

    pub fn add(&self, pubkey: Pubkey) -> Option<Pubkey> {
        match self.add_with_evict_filter(pubkey, |_| true) {
            AddAccountOutcome::Evicted(evicted) => Some(evicted),
            AddAccountOutcome::AlreadyPresent
            | AddAccountOutcome::Added
            | AddAccountOutcome::NoEvictableCandidate => None,
        }
    }

    pub fn add_with_evict_filter<F>(
        &self,
        pubkey: Pubkey,
        is_evictable: F,
    ) -> AddAccountOutcome
    where
        F: Fn(&Pubkey) -> bool,
    {
        // The cloning pipeline itself depends on some accounts that should
        // never be evicted.
        // Thus we ignore them here in order to never cause a removal/unsubscribe.
        if self.accounts_to_never_evict.contains(&pubkey) {
            trace!(pubkey = %pubkey, "Account is in the never-evict set, skipping");
            return AddAccountOutcome::Added;
        }

        let mut subs = self.subscribed_accounts.lock();
        // If the pubkey is already in the cache, we just promote it
        if subs.promote(&pubkey) {
            trace!(pubkey = %pubkey, "Account promoted");
            return AddAccountOutcome::AlreadyPresent;
        }
        trace!(pubkey = %pubkey, "Adding new account");

        let Some((evicted_pubkey, _)) = subs.push(pubkey, ()) else {
            return AddAccountOutcome::Added;
        };

        debug_assert_ne!(
            evicted_pubkey, pubkey,
            "Should not evict the same pubkey that we added"
        );

        fn restore_skipped(
            subs: &mut LruCache<Pubkey, ()>,
            skipped: Vec<Pubkey>,
        ) {
            for skipped_pubkey in skipped {
                subs.push(skipped_pubkey, ());
            }
        }

        fn reject_new_account(
            subs: &mut LruCache<Pubkey, ()>,
            skipped: Vec<Pubkey>,
            pubkey: &Pubkey,
        ) -> AddAccountOutcome {
            restore_skipped(subs, skipped);
            subs.pop(pubkey);
            trace!(
                pubkey = %pubkey,
                "No evictable LRU candidate found; rejected new account"
            );
            AddAccountOutcome::NoEvictableCandidate
        }

        let mut skipped = Vec::new();
        let mut candidate = evicted_pubkey;
        loop {
            if candidate == pubkey {
                return reject_new_account(&mut subs, skipped, &pubkey);
            }

            if is_evictable(&candidate) {
                restore_skipped(&mut subs, skipped);
                inc_evicted_accounts_count();
                trace!(evicted_pubkey = %candidate, "Evict candidate");
                return AddAccountOutcome::Evicted(candidate);
            }

            // Skipping delegated LRU candidate during capacity eviction; for example,
            // delegated accounts being undelegated, or delegation records kept for
            // pending clones, are cleaned up by delegated-state processing.
            trace!(
                skipped_pubkey = %candidate,
                "skipping delegated LRU candidate during capacity eviction; assuming transient direct-subscription entry, cleanup expected from delegated-state processing"
            );
            skipped.push(candidate);

            let Some((next_candidate, ())) = subs.pop_lru() else {
                return reject_new_account(&mut subs, skipped, &pubkey);
            };
            candidate = next_candidate;
        }
    }

    pub fn contains(&self, pubkey: &Pubkey) -> bool {
        let subs = self.subscribed_accounts.lock();
        subs.contains(pubkey)
    }

    pub fn remove(&self, pubkey: &Pubkey) -> bool {
        debug_assert!(
            !self.accounts_to_never_evict.contains(pubkey),
            "Cannot remove an account that is not supposed to be evicted: {pubkey}"
        );
        let mut subs = self.subscribed_accounts.lock();
        if subs.pop(pubkey).is_some() {
            trace!(pubkey = %pubkey, "Removed account");
            true
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        let subs = self.subscribed_accounts.lock();
        subs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn never_evicted_accounts(&self) -> Vec<Pubkey> {
        self.accounts_to_never_evict.iter().cloned().collect()
    }

    pub fn can_evict(&self, pubkey: &Pubkey) -> bool {
        !self.accounts_to_never_evict.contains(pubkey)
    }

    pub fn pubkeys(&self) -> HashSet<Pubkey> {
        let subs = self.subscribed_accounts.lock();
        subs.iter().map(|(k, _)| *k).collect()
    }
}

impl SubscribedAccountsTracker for AccountsLruCache {
    fn subscribed_accounts(&self) -> HashSet<Pubkey> {
        let subs = self.subscribed_accounts.lock();
        subs.iter().map(|(k, _)| *k).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::*;

    #[tokio::test]
    async fn test_lru_cache_add_accounts_up_to_limit_no_eviction() {
        let capacity = NonZeroUsize::new(3).unwrap();
        let cache = AccountsLruCache::new(capacity);

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();

        // Add three accounts (up to limit)
        let evicted1 = cache.add(pubkey1);
        let evicted2 = cache.add(pubkey2);
        let evicted3 = cache.add(pubkey3);

        // No evictions should occur
        assert_eq!(evicted1, None);
        assert_eq!(evicted2, None);
        assert_eq!(evicted3, None);
    }

    #[tokio::test]
    async fn test_lru_cache_add_same_account_multiple_times_no_eviction() {
        let capacity = NonZeroUsize::new(3).unwrap();
        let cache = AccountsLruCache::new(capacity);

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();

        // Add two different accounts first
        let evicted1 = cache.add(pubkey1);
        let evicted2 = cache.add(pubkey2);

        // Add the same accounts multiple times
        let evicted3 = cache.add(pubkey1); // Should just promote
        let evicted4 = cache.add(pubkey2); // Should just promote
        let evicted5 = cache.add(pubkey1); // Should just promote

        // No evictions should occur
        assert_eq!(evicted1, None);
        assert_eq!(evicted2, None);
        assert_eq!(evicted3, None);
        assert_eq!(evicted4, None);
        assert_eq!(evicted5, None);
    }

    #[tokio::test]
    async fn test_lru_cache_eviction_when_exceeding_limit() {
        let capacity = NonZeroUsize::new(3).unwrap();
        let cache = AccountsLruCache::new(capacity);

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();
        let pubkey4 = Pubkey::new_unique();

        // Fill cache to capacity
        cache.add(pubkey1);
        cache.add(pubkey2);
        cache.add(pubkey3);

        // Add a fourth account, which should evict the least recently used (pubkey1)
        let evicted = cache.add(pubkey4);
        assert_eq!(evicted, Some(pubkey1));
    }

    #[tokio::test]
    async fn test_lru_cache_lru_eviction_order() {
        let capacity = NonZeroUsize::new(3).unwrap();
        let cache = AccountsLruCache::new(capacity);

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();
        let pubkey4 = Pubkey::new_unique();
        let pubkey5 = Pubkey::new_unique();

        // Fill cache: [1, 2, 3] (1 is least recently used)
        cache.add(pubkey1);
        cache.add(pubkey2);
        cache.add(pubkey3);

        // Access pubkey1 to make it more recently used: [2, 3, 1]
        cache.add(pubkey1); // This should just promote, making order [2, 3, 1]

        // Add pubkey4, should evict pubkey2 (now least recently used)
        let evicted = cache.add(pubkey4);
        assert_eq!(evicted, Some(pubkey2));

        // Add pubkey5, should evict pubkey3 (now least recently used)
        let evicted = cache.add(pubkey5);
        assert_eq!(evicted, Some(pubkey3));
    }

    #[tokio::test]
    async fn test_lru_cache_multiple_evictions_in_sequence() {
        let capacity = NonZeroUsize::new(4).unwrap();
        let cache = AccountsLruCache::new(capacity);

        // Create test pubkeys
        let pubkeys: Vec<Pubkey> =
            (1..=7).map(|_| Pubkey::new_unique()).collect();

        // Fill cache to capacity (no evictions)
        for pk in pubkeys.iter().take(4) {
            let evicted = cache.add(*pk);
            assert_eq!(evicted, None);
        }

        // Add more accounts and verify evictions happen in LRU order
        for i in 4..7 {
            let evicted = cache.add(pubkeys[i]);
            let expected_evicted = pubkeys[i - 4]; // Should evict the account added 4 steps ago

            assert_eq!(evicted, Some(expected_evicted));
        }
    }

    #[tokio::test]
    async fn test_lru_cache_skips_protected_candidate_and_evicts_next() {
        let capacity = NonZeroUsize::new(3).unwrap();
        let cache = AccountsLruCache::new(capacity);

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();
        let pubkey4 = Pubkey::new_unique();

        cache.add(pubkey1);
        cache.add(pubkey2);
        cache.add(pubkey3);

        let outcome = cache.add_with_evict_filter(pubkey4, |pk| *pk != pubkey1);

        assert_eq!(outcome, AddAccountOutcome::Evicted(pubkey2));
        assert!(cache.contains(&pubkey1));
        assert!(cache.contains(&pubkey4));
        assert!(!cache.contains(&pubkey2));
    }

    #[tokio::test]
    async fn test_lru_cache_all_candidates_protected_rejects_new_account() {
        let capacity = NonZeroUsize::new(2).unwrap();
        let cache = AccountsLruCache::new(capacity);

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();

        cache.add(pubkey1);
        cache.add(pubkey2);

        let outcome = cache.add_with_evict_filter(pubkey3, |pk| {
            assert_ne!(*pk, pubkey3, "new account must not be evicted");
            false
        });

        assert_eq!(outcome, AddAccountOutcome::NoEvictableCandidate);
        assert!(cache.contains(&pubkey1));
        assert!(cache.contains(&pubkey2));
        assert!(!cache.contains(&pubkey3));
    }

    #[tokio::test]
    async fn test_lru_cache_skips_ephemeral_protected_candidate_and_evicts_next(
    ) {
        let capacity = NonZeroUsize::new(3).unwrap();
        let cache = AccountsLruCache::new(capacity);

        let ephemeral_pubkey = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();
        let pubkey4 = Pubkey::new_unique();

        cache.add(ephemeral_pubkey);
        cache.add(pubkey2);
        cache.add(pubkey3);

        let outcome =
            cache.add_with_evict_filter(pubkey4, |pk| *pk != ephemeral_pubkey);

        assert_eq!(outcome, AddAccountOutcome::Evicted(pubkey2));
        assert!(cache.contains(&ephemeral_pubkey));
        assert!(cache.contains(&pubkey4));
        assert!(!cache.contains(&pubkey2));
    }

    #[test]
    fn test_never_evicted_accounts() {
        let capacity = NonZeroUsize::new(3).unwrap();
        let cache = AccountsLruCache::new(capacity);

        let never_evicted = cache.never_evicted_accounts();
        // Should contain at least the clock sysvar
        assert!(never_evicted.contains(&sysvar::clock::id()));
    }
}
