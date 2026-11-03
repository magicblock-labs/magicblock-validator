use std::collections::HashSet;

use solana_pubkey::Pubkey;

/// Tracks and provides the current set of subscribed accounts.
///
/// This trait abstracts the source of subscription state, allowing SubMuxClient
/// to remain decoupled from the specific implementation (e.g., subscription set).
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
}

#[cfg(test)]
pub(super) mod mock {
    use parking_lot::Mutex;

    use super::*;

    /// A simple mock implementation for testing that allows setting
    /// subscriptions before reconnect operations.
    ///
    /// The stored subscriptions should be unique to comply with the
    /// `SubscribedAccountsTracker` trait contract.
    pub(crate) struct MockSubscribedAccountsTracker {
        subscriptions: Mutex<Vec<Pubkey>>,
    }

    impl MockSubscribedAccountsTracker {
        pub(crate) fn new(subscriptions: Vec<Pubkey>) -> Self {
            Self {
                subscriptions: Mutex::new(subscriptions),
            }
        }

        #[allow(dead_code)]
        pub(crate) fn set_subscriptions(&self, subscriptions: Vec<Pubkey>) {
            *self.subscriptions.lock() = subscriptions;
        }
    }

    impl SubscribedAccountsTracker for MockSubscribedAccountsTracker {
        fn subscribed_accounts(&self) -> HashSet<Pubkey> {
            self.subscriptions.lock().iter().copied().collect()
        }
    }
}
