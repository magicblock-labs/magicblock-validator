use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use solana_sdk::{clock::Slot, pubkey::Pubkey};

use crate::{AccountUpdates, AccountUpdatesResult};

#[derive(Debug, Clone, Default)]
pub struct AccountUpdatesStub {
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

#[allow(unused)] // used in tests
impl AccountUpdatesStub {
    pub fn add_known_update_slot(&mut self, pubkey: Pubkey, at_slot: Slot) {
        self.last_known_update_slots.write().expect("RwLock of AccountUpdatesStub.last_known_update_slots is poisoned").insert(pubkey, at_slot);
    }
}

impl AccountUpdates for AccountUpdatesStub {
    fn ensure_account_monitoring(
        &self,
        _pubkey: &Pubkey,
    ) -> AccountUpdatesResult<()> {
        Ok(())
    }
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.last_known_update_slots.read().expect("RwLock of AccountUpdatesStub.last_known_update_slots is poisoned").get(pubkey).cloned()
    }
}
