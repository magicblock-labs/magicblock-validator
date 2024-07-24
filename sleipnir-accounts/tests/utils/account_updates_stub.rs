use std::collections::HashMap;

use sleipnir_account_updates::AccountUpdates;
use solana_sdk::{clock::Slot, pubkey::Pubkey};

#[derive(Debug, Default, Clone)]
pub struct AccountUpdatesStub {
    pub last_update_slots: HashMap<Pubkey, Slot>,
}

impl AccountUpdatesStub {
    pub fn add_known_update(&mut self, pubkey: &Pubkey, slot: Slot) {
        let previous_last_update_slot = self.last_update_slots.remove(pubkey);
        if let Some(previous_last_update_slot) = previous_last_update_slot {
            if previous_last_update_slot > slot {
                self.last_update_slots
                    .insert(*pubkey, previous_last_update_slot);
                return;
            }
        }
        self.last_update_slots.insert(*pubkey, slot);
    }
}

impl AccountUpdates for AccountUpdatesStub {
    fn ensure_monitoring_of_account(&self, _pubkey: &Pubkey) {
        // Noop
    }
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: Slot) -> bool {
        if let Some(last_update_slot) = self.last_update_slots.get(pubkey) {
            *last_update_slot > slot
        } else {
            false
        }
    }
}

impl AccountUpdatesStub {}
