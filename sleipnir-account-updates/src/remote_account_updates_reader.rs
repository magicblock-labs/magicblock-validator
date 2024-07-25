use log::*;
use solana_sdk::{clock::Slot, pubkey::Pubkey};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    AccountUpdates, RemoteAccountUpdatesWatcher,
    RemoteAccountUpdatesWatcherRequest,
};

pub struct RemoteAccountUpdatesReader {
    last_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    request_sender: UnboundedSender<RemoteAccountUpdatesWatcherRequest>,
}

impl RemoteAccountUpdatesReader {
    pub fn new(watcher: &RemoteAccountUpdatesWatcher) -> Self {
        Self {
            last_update_slots: watcher.get_last_update_slots(),
            request_sender: watcher.get_request_sender(),
        }
    }
}

impl AccountUpdates for RemoteAccountUpdatesReader {
    fn request_account_monitoring(&self, pubkey: &Pubkey) {
        if let Err(error) =
            self.request_sender
                .send(RemoteAccountUpdatesWatcherRequest {
                    account: pubkey.clone(),
                })
        {
            error!(
                "Failed to request monitoring of account: {}: {:?}",
                pubkey, error
            )
        }
    }
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: Slot) -> bool {
        if let Some(last_update_slot) =
            self.last_update_slots.read().unwrap().get(pubkey)
        {
            *last_update_slot > slot
        } else {
            false
        }
    }
}
