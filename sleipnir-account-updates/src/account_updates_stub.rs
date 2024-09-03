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
    pub fn set_known_update_slot(&self, pubkey: Pubkey, at_slot: Slot) {
        self.last_known_update_slots
            .write()
            .unwrap()
            .insert(pubkey, at_slot);
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
        self.last_known_update_slots
            .read()
            .unwrap()
            .get(pubkey)
            .cloned()
    }
}
