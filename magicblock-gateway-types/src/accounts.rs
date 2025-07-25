use solana_account::{cow::AccountSeqLock, AccountSharedData};
use solana_pubkey::Pubkey;
use tokio::sync::mpsc::{Receiver, Sender};

pub type AccountUpdateRx = Receiver<LockedAccount>;
pub type AccountUpdateTx = Sender<LockedAccount>;

pub struct LockedAccount {
    pub pubkey: Pubkey,
    pub lock: Option<AccountSeqLock>,
    pub account: AccountSharedData,
}

impl LockedAccount {
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
