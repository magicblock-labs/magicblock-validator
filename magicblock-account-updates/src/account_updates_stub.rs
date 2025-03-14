use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use solana_sdk::{clock::Slot, pubkey::Pubkey};

use crate::{AccountUpdates, AccountUpdatesResult};

#[derive(Debug, Clone, Default)]
pub struct AccountUpdatesStub {
    account_monitoring: Arc<RwLock<HashSet<Pubkey>>>,
    first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl AccountUpdatesStub {
    pub fn has_account_monitoring(&self, pubkey: &Pubkey) -> bool {
        self.account_monitoring.read().unwrap().contains(pubkey)
    }
    pub fn set_first_subscribed_slot(&self, pubkey: Pubkey, at_slot: Slot) {
        self.first_subscribed_slots
            .write()
            .unwrap()
            .insert(pubkey, at_slot);
    }
    pub fn set_last_known_update_slot(&self, pubkey: Pubkey, at_slot: Slot) {
        self.last_known_update_slots
            .write()
            .unwrap()
            .insert(pubkey, at_slot);
    }
}

impl AccountUpdates for AccountUpdatesStub {
    async fn ensure_account_monitoring(
        &self,
        pubkey: &Pubkey,
    ) -> AccountUpdatesResult<()> {
        self.account_monitoring.write().unwrap().insert(*pubkey);
        Ok(())
    }
    fn get_first_subscribed_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.first_subscribed_slots
            .read()
            .unwrap()
            .get(pubkey)
            .cloned()
    }
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.last_known_update_slots
            .read()
            .unwrap()
            .get(pubkey)
            .cloned()
    }
}
