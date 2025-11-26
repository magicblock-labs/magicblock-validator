use solana_pubkey::Pubkey;

/// Tracks and provides the current set of subscribed accounts.
///
/// This trait abstracts the source of subscription state, allowing SubMuxClient
/// to remain decoupled from the specific implementation (e.g., LRU cache).
/// The reconnect logic queries this tracker to determine which accounts to
/// resubscribe when a client reconnects after being disconnected.
pub trait SubscribedAccountsTracker: Send + Sync + 'static {
    /// Returns the list of pubkeys that are currently subscribed to.
    fn subscribed_accounts(&self) -> Vec<Pubkey>;
}

#[cfg(test)]
pub mod mock {
    use std::sync::Mutex;

    use super::*;

    /// A simple mock implementation for testing that allows setting
    /// subscriptions before reconnect operations.
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
        fn subscribed_accounts(&self) -> Vec<Pubkey> {
            self.subscriptions.lock().unwrap().clone()
        }
    }
}
