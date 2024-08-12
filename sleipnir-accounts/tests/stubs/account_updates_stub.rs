use std::collections::HashMap;

use sleipnir_account_updates::AccountUpdates;
use solana_sdk::{clock::Slot, pubkey::Pubkey};

#[derive(Debug, Default, Clone)]
pub struct AccountUpdatesStub {
    last_known_update_slots: HashMap<Pubkey, Slot>,
}

#[allow(unused)] // used in tests
impl AccountUpdatesStub {
    pub fn add_known_update_slot(&mut self, pubkey: Pubkey, at_slot: Slot) {
        self.last_known_update_slots.insert(pubkey, at_slot);
    }
}

impl AccountUpdates for AccountUpdatesStub {
    fn request_start_account_monitoring(&self, _pubkey: &Pubkey) {
        // Noop for stub
    }
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.last_known_update_slots.get(pubkey).cloned()
    }
}
