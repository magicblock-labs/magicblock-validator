use std::collections::HashSet;

use solana_pubkey::Pubkey;

/// Tracks and provides the current set of subscribed accounts.
///
/// This trait abstracts the source of subscription state, allowing SubMuxClient
/// to remain decoupled from the specific implementation (e.g., LRU cache).
/// The reconnect logic queries this tracker to determine which accounts to
/// resubscribe when a client reconnects after being disconnected.
///
/// Implementors must return a set (no duplicates) of currently subscribed
/// accounts.
pub trait SubscribedAccountsTracker: Send + Sync + 'static {
    /// Returns the set of pubkeys that are currently subscribed to.
    ///
    /// Each pubkey appears at most once in the returned set.
    fn subscribed_accounts(&self) -> HashSet<Pubkey>;

    /// Returns whether one pubkey is currently subscribed.
    ///
    /// Implementors backed by a keyed collection should override this to
    /// avoid cloning the complete subscription set during attach catch-up.
    fn contains(&self, pubkey: &Pubkey) -> bool {
        self.subscribed_accounts().contains(pubkey)
    }
}

#[cfg(test)]
pub mod mock {
    use std::sync::Mutex;

    use super::*;

    /// A simple mock implementation for testing that allows setting
    /// subscriptions before reconnect operations.
    ///
    /// The stored subscriptions should be unique to comply with the
    /// `SubscribedAccountsTracker` trait contract.
    pub struct MockSubscribedAccountsTracker {
        subscriptions: Mutex<Vec<Pubkey>>,
    }

    impl MockSubscribedAccountsTracker {
        pub fn new(subscriptions: Vec<Pubkey>) -> Self {
            Self {
                subscriptions: Mutex::new(subscriptions),
            }
        }

        #[allow(dead_code)]
        pub fn set_subscriptions(&self, subscriptions: Vec<Pubkey>) {
            *self.subscriptions.lock().unwrap() = subscriptions;
        }
    }

    impl SubscribedAccountsTracker for MockSubscribedAccountsTracker {
        fn subscribed_accounts(&self) -> HashSet<Pubkey> {
            self.subscriptions.lock().unwrap().iter().copied().collect()
        }

        fn contains(&self, pubkey: &Pubkey) -> bool {
            self.subscriptions.lock().unwrap().contains(pubkey)
        }
    }
}
