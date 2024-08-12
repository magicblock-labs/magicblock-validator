use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use log::*;
use solana_sdk::{clock::Slot, pubkey::Pubkey};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    AccountUpdates, RemoteAccountUpdatesRequest, RemoteAccountUpdatesWorker,
};

pub struct RemoteAccountUpdatesClient {
    request_sender: UnboundedSender<RemoteAccountUpdatesRequest>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesClient {
    pub fn new(worker: &RemoteAccountUpdatesWorker) -> Self {
        Self {
            request_sender: worker.get_request_sender(),
            last_known_update_slots: worker.get_last_known_update_slots(),
        }
    }
}

impl AccountUpdates for RemoteAccountUpdatesClient {
    fn request_start_account_monitoring(&self, pubkey: &Pubkey) {
        if let Err(error) = self
            .request_sender
            .send(RemoteAccountUpdatesRequest { account: *pubkey })
        {
            error!(
                "Failed to request monitoring of account: {}: {:?}",
                pubkey, error
            )
        }
    }
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.last_known_update_slots
            .read()
            .unwrap()
            .get(pubkey)
            .cloned()
    }
}
