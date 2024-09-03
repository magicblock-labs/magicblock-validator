use solana_sdk::{account::AccountSharedData, clock::Slot, pubkey::Pubkey};

pub trait InternalAccountProvider: Send + Sync {
    fn has_account(&self, pubkey: &Pubkey) -> bool;
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
    fn get_slot(&self) -> Slot;
}
