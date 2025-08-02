use std::sync::Arc;

use flume::{Receiver as MpmcReceiver, Sender as MpmcSender};
use solana_account::cow::AccountSeqLock;
use solana_account_decoder::{encode_ui_account, UiAccount, UiAccountEncoding};
use tokio::sync::{
    mpsc::{Receiver, Sender},
    Notify,
};

pub use solana_account::{AccountSharedData, ReadableAccount};
pub use solana_pubkey::Pubkey;

use crate::Slot;

/// Receiving end of account updates channel
pub type AccountUpdateRx = MpmcReceiver<AccountWithSlot>;
/// Sending end of account updates channel
pub type AccountUpdateTx = MpmcSender<AccountWithSlot>;

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

pub struct AccountWithSlot {
    pub account: LockedAccount,
    pub slot: Slot,
}

impl AccountsToEnsure {
    pub fn new(accounts: Vec<Pubkey>) -> Self {
        let ready = Arc::default();
        let accounts = accounts.into_boxed_slice();
        Self { accounts, ready }
    }
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

    #[inline]
    pub fn ui_encode(&self, encoding: UiAccountEncoding) -> UiAccount {
        self.read_locked(|pk, acc| {
            encode_ui_account(pk, acc, encoding, None, None)
        })
    }

    #[inline]
    fn changed(&self) -> bool {
        self.lock
            .as_ref()
            .map(|lock| lock.changed())
            .unwrap_or_default()
    }

    pub fn read_locked<F, R>(&self, reader: F) -> R
    where
        F: Fn(&Pubkey, &AccountSharedData) -> R,
    {
        let result = reader(&self.pubkey, &self.account);
        if !self.changed() {
            return result;
        }
        let AccountSharedData::Borrowed(ref borrowed) = self.account else {
            return result;
        };
        let Some(mut lock) = self.lock.clone() else {
            return result;
        };
        let mut account = borrowed.reinit();
        loop {
            let result = reader(&self.pubkey, &account);
            if lock.changed() {
                account = borrowed.reinit();
                lock.relock();
                continue;
            }
            break result;
        }
    }
}
