use solana_sdk::{clock::Slot, pubkey::Pubkey};

pub trait AccountUpdates {
    fn request_start_account_monitoring(&self, pubkey: &Pubkey);
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot>;
}
