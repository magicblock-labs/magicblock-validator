use log::*;
use scc::HashCache;
use solana_sdk::sysvar;
use std::collections::HashSet;

use solana_pubkey::Pubkey;

/// A simple wrapper around [lru::LruCache].
/// When an account is evicted from the cache due to a new one being added,
/// it will return that evicted account's Pubkey as well as sending it via
/// the [Self::removed_account_rx] channel.
pub struct AccountsLruCache {
    /// Tracks which accounts are currently subscribed to
    subscribed_accounts: HashCache<Pubkey, ()>,
    accounts_to_never_evict: HashSet<Pubkey>,
}

fn accounts_to_never_evict() -> HashSet<Pubkey> {
    let mut set = HashSet::new();
    set.insert(sysvar::clock::id());
    set
}

impl AccountsLruCache {
    pub fn new(capacity: usize) -> Self {
        let accounts_to_never_evict = accounts_to_never_evict();
        Self {
            subscribed_accounts: HashCache::with_capacity(capacity, capacity),
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

        for &key in pubkeys {
            self.subscribed_accounts.get(key);
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

        // If the pubkey is already in the cache, we just promote it
        if self.subscribed_accounts.get(&pubkey).is_some() {
            trace!("Account promoted: {pubkey}");
            return None;
        }
        trace!("Adding new account: {pubkey}");

        // Otherwise we add it new and possibly deal with an eviction
        // on the caller side
        let evicted = self
            .subscribed_accounts
            .put(pubkey, ())
            .ok()
            .flatten()
            .map(|(k, _)| k);

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
        self.subscribed_accounts.contains(pubkey)
    }

    pub fn remove(&self, pubkey: &Pubkey) -> bool {
        debug_assert!(
            !self.accounts_to_never_evict.contains(pubkey),
            "Cannot remove an account that is not supposed to be evicted: {pubkey}"
        );
        self.subscribed_accounts.remove(pubkey).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The minimum capacity for scc::HashCache is 64. We use a higher value
    /// to make statistical outcomes in tests highly predictable.
    const CAPACITY: usize = 512;

    /// Tests basic insertion and upsert functionality well below the capacity limit.
    #[test]
    fn test_add_and_upsert_within_capacity() {
        let cache = AccountsLruCache::new(CAPACITY);
        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();

        cache.add(pubkey1);
        cache.add(pubkey2);
        assert_eq!(cache.subscribed_accounts.len(), 2);

        // Re-adding (upserting) an existing key should not change the size
        // and should not cause an eviction of other keys.
        let evicted = cache.add(pubkey1);
        assert_eq!(evicted, None, "Upserting should not cause an eviction");
        assert_eq!(
            cache.subscribed_accounts.len(),
            2,
            "Cache size should not change on upsert"
        );
    }

    /// Verifies that the cache's size is strictly bounded by its capacity,
    /// even after a large number of insertions.
    #[test]
    fn test_cache_size_is_bounded_after_many_insertions() {
        let cache = AccountsLruCache::new(CAPACITY);

        // Insert three times the capacity, forcing many evictions.
        for _ in 0..(CAPACITY * 3) {
            cache.add(Pubkey::new_unique());
        }

        // The primary guarantee is that the cache does not grow beyond its limit.
        // After this many insertions, it should be exactly full.
        assert_eq!(
            cache.subscribed_accounts.len(),
            CAPACITY,
            "Cache length should be at capacity after many insertions"
        );
    }

    /// This test covers a hybrid scenario:
    /// 1. A phase with no evictions.
    /// 2. A phase where evictions are guaranteed.
    #[test]
    fn test_hybrid_no_eviction_then_eviction() {
        let cache = AccountsLruCache::new(CAPACITY);

        // --- Phase 1: No Evictions ---
        // Add half the capacity. Evictions are statistically impossible here.
        for _ in 0..(CAPACITY / 2) {
            cache.add(Pubkey::new_unique());
        }
        assert_eq!(cache.subscribed_accounts.len(), CAPACITY / 2);

        // --- Phase 2: Guaranteed Evictions ---
        // Add more items to fill the cache and force evictions.
        let mut eviction_count = 0;
        for _ in 0..(CAPACITY) {
            // Add enough to definitely overflow
            if cache.add(Pubkey::new_unique()).is_some() {
                eviction_count += 1;
            }
        }

        // After the churn, the cache should be full.
        assert_eq!(cache.subscribed_accounts.len(), CAPACITY);
        assert!(
            eviction_count > 0,
            "At least one eviction should have occurred in the second phase"
        );
    }

    /// Tests that `promote_multi` makes keys "stickier" and far less likely to be
    /// evicted compared to non-promoted keys during churn.
    #[test]
    fn test_promote_multi_dramatically_reduces_eviction_likelihood() {
        let cache = AccountsLruCache::new(CAPACITY);
        let keys: Vec<Pubkey> =
            (0..CAPACITY).map(|_| Pubkey::new_unique()).collect();

        // 1. Populate the cache completely.
        for key in &keys {
            cache.add(*key);
        }

        // 2. Divide keys into two groups: those to promote and those to leave alone.
        let (keys_to_promote, keys_not_promoted) = keys.split_at(CAPACITY / 2);
        let promote_refs: Vec<&Pubkey> = keys_to_promote
            .iter()
            .filter(|k| cache.contains(k))
            .collect();

        // 4. Force heavy churn by replacing 100% of the cache capacity with new items.
        for _ in 0..CAPACITY {
            // Continuously promote the first group, making them "hot".
            cache.promote_multi(&promote_refs);
            cache.add(Pubkey::new_unique());
        }

        // 5. Verify the survival rates.
        let promoted_survivors =
            keys_to_promote.iter().filter(|k| cache.contains(k)).count();
        let non_promoted_survivors = keys_not_promoted
            .iter()
            .filter(|k| cache.contains(k))
            .count();

        let promoted_survival_rate =
            promoted_survivors as f64 / promote_refs.len() as f64;
        let non_promoted_survival_rate =
            non_promoted_survivors as f64 / keys_not_promoted.len() as f64;

        // Assert that the promoted keys had a very high survival rate.
        assert!(
            promoted_survival_rate > 0.95,
            "Expected a very high survival rate (>95%) for promoted keys, but got {:.2}",
            promoted_survival_rate
        );

        // Assert that promotion provided a significant advantage.
        assert!(
            promoted_survival_rate > non_promoted_survival_rate,
            "Promoted keys should have a higher survival rate than non-promoted ones"
        );
    }
}
