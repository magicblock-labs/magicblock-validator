use std::{collections::HashSet, num::NonZeroUsize, sync::Mutex};

use log::*;
use lru::LruCache;
use solana_pubkey::Pubkey;
use solana_sdk::sysvar;

/// A simple wrapper around [lru::LruCache].
/// When an account is evicted from the cache due to a new one being added,
/// it will return that evicted account's Pubkey as well as sending it via
/// the [Self::removed_account_rx] channel.
pub struct AccountsLruCache {
    /// Tracks which accounts are currently subscribed to
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
        if log::log_enabled!(log::Level::Trace) {
            let pubkeys = pubkeys
                .iter()
                .map(|pk| pk.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trace!("Promoting: {pubkeys}");
        }

        let mut subs = self
            .subscribed_accounts
            .lock()
            .expect("subscribed_accounts lock poisoned");
        for key in pubkeys {
            subs.promote(key);
        }
    }

    pub fn add(&self, pubkey: Pubkey) -> Option<Pubkey> {
        // The cloning pipeline itself depends on some accounts that should
        // never be evicted.
        // Thus we ignore them here in order to never cause a removal/unsubscribe.
        if self.accounts_to_never_evict.contains(&pubkey) {
            trace!("Account {pubkey} is in the never-evict set, skipping");
            return None;
        }

        let mut subs = self
            .subscribed_accounts
            .lock()
            .expect("subscribed_accounts lock poisoned");
        // If the pubkey is already in the cache, we just promote it
        if subs.promote(&pubkey) {
            trace!("Account promoted: {pubkey}");
            return None;
        }
        trace!("Adding new account: {pubkey}");

        // Otherwise we add it new and possibly deal with an eviction
        // on the caller side
        let evicted = subs
            .push(pubkey, ())
            .map(|(evicted_pubkey, _)| evicted_pubkey);

        if let Some(evicted_pubkey) = evicted {
            debug_assert_ne!(
                evicted_pubkey, pubkey,
                "Should not evict the same pubkey that we added"
            );
            trace!("Evict candidate: {evicted_pubkey}");
        }

        evicted
    }

    pub fn contains(&self, pubkey: &Pubkey) -> bool {
        let subs = self
            .subscribed_accounts
            .lock()
            .expect("subscribed_accounts lock poisoned");
        subs.contains(pubkey)
    }

    pub fn remove(&self, pubkey: &Pubkey) -> bool {
        debug_assert!(
            !self.accounts_to_never_evict.contains(pubkey),
            "Cannot remove an account that is not supposed to be evicted: {pubkey}"
        );
        let mut subs = self
            .subscribed_accounts
            .lock()
            .expect("subscribed_accounts lock poisoned");
        if subs.pop(pubkey).is_some() {
            trace!("Removed account: {pubkey}");
            true
        } else {
            false
        }
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
}
