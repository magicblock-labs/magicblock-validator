use flume::{Receiver as MpmcReceiver, Sender as MpmcSender};
use solana_account::{cow::AccountSeqLock, AccountSharedData};
use solana_account_decoder::{
    encode_ui_account, UiAccount, UiAccountEncoding, UiDataSliceConfig,
};
use solana_pubkey::Pubkey;

use crate::Slot;

/// The receiving end of the channel for account state changes.
pub type AccountUpdateRx = MpmcReceiver<AccountWithSlot>;
/// The sending end of the channel for account state changes.
pub type AccountUpdateTx = MpmcSender<AccountWithSlot>;

/// A message that bundles an updated account with the slot in which the update occurred.
pub struct AccountWithSlot {
    pub account: LockedAccount,
    pub slot: Slot,
}

/// A wrapper for account data that provides a mechanism for safe, optimistic concurrent reads.
///
/// When an account's data is `Borrowed`, it points to memory that can be modified by another
/// thread. This struct uses a sequence lock (`AccountSeqLock`) to detect if a concurrent
/// modification occurred during a read operation, allowing the read to be safely retried.
pub struct LockedAccount {
    /// The public key of the account.
    pub pubkey: Pubkey,
    /// A sequence lock captured at the time of creation. It is `Some` only for `Borrowed`
    /// accounts and is used to detect read-write race conditions.
    pub lock: Option<AccountSeqLock>,
    /// The account's data, which can be either owned or a borrowed reference.
    pub account: AccountSharedData,
}

impl LockedAccount {
    /// Creates a new `LockedAccount`, capturing the initial sequence lock state
    /// if the account data is borrowed.
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

    /// Safely reads the account data and encodes it into the `UiAccount` format for RPC responses.
    /// This method internally uses `read_locked` to ensure data consistency.
    #[inline]
    pub fn ui_encode(
        &self,
        encoding: UiAccountEncoding,
        slice: Option<UiDataSliceConfig>,
    ) -> UiAccount {
        self.read_locked(|pk, acc| {
            encode_ui_account(pk, acc, encoding, None, slice)
        })
    }

    /// Checks the sequence lock to see if the underlying data has been modified since this
    /// `LockedAccount` was created. Returns `false` for `Owned` accounts.
    #[inline]
    fn changed(&self) -> bool {
        self.lock
            .as_ref()
            .map(|lock| lock.changed())
            .unwrap_or_default()
    }

    /// Performs a read operation on the account data, automatically handling race conditions.
    ///
    /// ## How it Works
    /// This function implements an optimistic read pattern:
    /// 1.  It executes the `reader` closure with the current account data.
    /// 2.  It then checks the sequence lock. If the data has not been changed concurrently
    ///     during the read, the result is returned immediately (the "fast path").
    /// 3.  If a race condition is detected, it enters a retry loop. It continuously
    ///     re-reads the latest account data and checks the lock again until a consistent,
    ///     race-free read can be completed.
    pub fn read_locked<F, R>(&self, reader: F) -> R
    where
        F: Fn(&Pubkey, &AccountSharedData) -> R,
    {
        // Optimistic read first.
        let result = reader(&self.pubkey, &self.account);

        if !self.changed() {
            return result;
        }

        // Slow path: must be Borrowed data.
        let AccountSharedData::Borrowed(ref borrowed) = self.account else {
            return result;
        };

        // Retry loop: always acquire a fresh lock *and* a fresh buffer view.
        let mut account = borrowed.reinit();

        loop {
            // Fresh lock snapshot *every attempt*
            let lock = borrowed.lock();

            let result = reader(&self.pubkey, &account);

            if lock.changed() {
                // Data changed during the read, try again with a fresh view
                account = borrowed.reinit();
                continue;
            }

            // Clean success
            break result;
        }
    }
}
