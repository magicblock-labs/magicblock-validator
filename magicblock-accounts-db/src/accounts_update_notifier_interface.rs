use std::sync::Arc;

use solana_sdk::{
    account::AccountSharedData, clock::Slot, pubkey::Pubkey,
    transaction::SanitizedTransaction,
};

pub trait AccountsUpdateNotifierInterface: std::fmt::Debug {
    /// Notified when an account is updated at runtime, due to transaction activities
    fn notify_account_update(
        &self,
        slot: Slot,
        account: &AccountSharedData,
        txn: &Option<&SanitizedTransaction>,
        pubkey: &Pubkey,
        write_version: u64,
    );
}

pub type AccountsUpdateNotifier =
    Arc<dyn AccountsUpdateNotifierInterface + Sync + Send>;
