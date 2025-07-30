use std::sync::Arc;

use solana_account::cow::AccountSeqLock;
use tokio::sync::{
    mpsc::{Receiver, Sender},
    Notify,
};

pub use solana_account::{AccountSharedData, ReadableAccount};
pub use solana_pubkey::Pubkey;

/// Receiving end of account updates channel
pub type AccountUpdateRx = Receiver<LockedAccount>;
/// Sending end of account updates channel
pub type AccountUpdateTx = Sender<LockedAccount>;

/// Receiving end of the channel for messages to ensure accounts
pub type EnsureAccountsRx = Receiver<AccountsToEnsure>;
/// Sending end of the channel for messages to ensure accounts
pub type EnsureAccountsTx = Sender<AccountsToEnsure>;

/// List of accounts to ensure for presence in the accounts database
pub struct AccountsToEnsure {
    /// List of accounts
    pub accounts: Box<[Pubkey]>,
    /// Notification handle, to signal the waiters that accounts' presence check is complete
    pub ready: Arc<Notify>,
}

/// Account state after transaction execution. The optional locking mechanism ensures that for
/// AccountSharedData::Borrowed variant, the reader has the ability to detect that the account has
/// been modified between locking and reading and retry the read if that's the case.
pub struct LockedAccount {
    /// Pubkey of the modified account
    pub pubkey: Pubkey,
    /// Sequence lock, optimistically allows to read the borrowed account,
    /// and handle concurrent modification post factum and retry
    pub lock: Option<AccountSeqLock>,
    /// Account state, either borrowed, or owned
    pub account: AccountSharedData,
}

impl LockedAccount {
    /// Construct new potenitally sequence locked account, record the lock state for Borrowed state
    pub fn new(pubkey: Pubkey, account: AccountSharedData) -> Self {
        let lock = match &account {
            AccountSharedData::Owned(_) => None,
            AccountSharedData::Borrowed(acc) => acc.lock().into(),
        };
        Self {
            lock,
            account,
            pubkey,
        }
    }
}
