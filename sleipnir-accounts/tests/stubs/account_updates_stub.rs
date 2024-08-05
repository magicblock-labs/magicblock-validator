use std::collections::HashMap;

use sleipnir_account_updates::AccountUpdates;
use solana_sdk::{clock::Slot, pubkey::Pubkey};

#[derive(Debug, Default, Clone)]
pub struct AccountUpdatesStub {
    last_update_slots: HashMap<Pubkey, Slot>,
}

#[allow(unused)] // used in tests
impl AccountUpdatesStub {
    pub fn add_known_update(&mut self, pubkey: Pubkey, slot: Slot) {
        self.last_update_slots.insert(pubkey, slot);
    }
}

impl AccountUpdates for AccountUpdatesStub {
    fn request_account_monitoring(&self, _pubkey: &Pubkey) {
        // Noop for stub
    }
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: Slot) -> bool {
        if let Some(last_update_slot) = self.last_update_slots.get(pubkey) {
            *last_update_slot > slot
        } else {
            false
        }
    }
}
