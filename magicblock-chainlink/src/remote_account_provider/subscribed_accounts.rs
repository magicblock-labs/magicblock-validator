use std::collections::HashSet;

use parking_lot::Mutex;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar;
use tracing::trace;

use crate::submux::SubscribedAccountsTracker;

/// Accounts with a live direct remote subscription.
///
/// Capacity and recency belong to the engine account cache. This set only
/// mirrors subscription presence so reconnect and reconciliation logic can
/// restore the subscriptions Chainlink still owns.
pub struct SubscribedAccounts {
    accounts: Mutex<HashSet<Pubkey>>,
    internally_managed: HashSet<Pubkey>,
}

impl Default for SubscribedAccounts {
    fn default() -> Self {
        Self {
            accounts: Mutex::new(HashSet::new()),
            internally_managed: HashSet::from([sysvar::clock::id()]),
        }
    }
}

impl SubscribedAccounts {
    pub fn add(&self, pubkey: Pubkey) -> bool {
        if self.internally_managed.contains(&pubkey) {
            return false;
        }
        let added = self.accounts.lock().insert(pubkey);
        if added {
            trace!(pubkey = %pubkey, "Tracking subscribed account");
        }
        added
    }

    pub fn contains(&self, pubkey: &Pubkey) -> bool {
        self.accounts.lock().contains(pubkey)
    }

    pub fn remove(&self, pubkey: &Pubkey) -> bool {
        if self.internally_managed.contains(pubkey) {
            return false;
        }
        let removed = self.accounts.lock().remove(pubkey);
        if removed {
            trace!(pubkey = %pubkey, "Stopped tracking subscribed account");
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.accounts.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn internally_managed(&self) -> Vec<Pubkey> {
        self.internally_managed.iter().copied().collect()
    }

    pub fn is_internally_managed(&self, pubkey: &Pubkey) -> bool {
        self.internally_managed.contains(pubkey)
    }

    pub fn pubkeys(&self) -> HashSet<Pubkey> {
        self.accounts.lock().clone()
    }
}

impl SubscribedAccountsTracker for SubscribedAccounts {
    fn subscribed_accounts(&self) -> HashSet<Pubkey> {
        self.pubkeys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_subscription_presence() {
        let subscriptions = SubscribedAccounts::default();
        let pubkey = Pubkey::new_unique();

        assert!(subscriptions.add(pubkey));
        assert!(!subscriptions.add(pubkey));
        assert!(subscriptions.contains(&pubkey));
        assert!(subscriptions.remove(&pubkey));
        assert!(subscriptions.is_empty());
    }

    #[test]
    fn clock_is_managed_outside_direct_subscriptions() {
        let subscriptions = SubscribedAccounts::default();
        let clock = sysvar::clock::id();

        assert!(!subscriptions.add(clock));
        assert!(!subscriptions.contains(&clock));
        assert_eq!(subscriptions.internally_managed(), vec![clock]);
    }
}
