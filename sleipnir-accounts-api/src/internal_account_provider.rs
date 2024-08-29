use solana_sdk::{account::AccountSharedData, pubkey::Pubkey};

pub trait InternalAccountProvider: Send + Sync {
    fn has_account(&self, pubkey: &Pubkey) -> bool;
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
}
