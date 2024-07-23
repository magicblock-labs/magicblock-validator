use log::*;
use solana_sdk::pubkey::Pubkey;

use crate::{RemoteAccountUpdates, RemoteAccountUpdatesWatching};

pub trait AccountUpdates {
    fn ensure_monitoring_of_account(&self, pubkey: &Pubkey);
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: u64) -> bool;
}

impl AccountUpdates for RemoteAccountUpdates {
    fn ensure_monitoring_of_account(&self, pubkey: &Pubkey) {
        if let Err(error) =
            self.monitoring_sender.send(RemoteAccountUpdatesWatching {
                account: pubkey.clone(),
            })
        {
            error!(
                "Failed to update monitoring of account: {}: {:?}",
                pubkey, error
            )
        }
    }
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: u64) -> bool {
        let last_update_slots_read = self.last_update_slots.read().unwrap();
        if let Some(last_update_slot) = last_update_slots_read.get(pubkey) {
            *last_update_slot > slot
        } else {
            false
        }
    }
}
