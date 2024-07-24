use solana_sdk::{clock::Slot, pubkey::Pubkey};

pub trait AccountUpdates {
    fn request_account_monitoring(&self, pubkey: &Pubkey);
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: Slot) -> bool;
}
