use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use solana_sdk::{clock::Slot, pubkey::Pubkey};
use tokio::sync::mpsc::UnboundedSender;

use crate::{AccountUpdates, AccountUpdatesError, RemoteAccountUpdatesWorker};

pub struct RemoteAccountUpdatesClient {
    request_sender: UnboundedSender<Pubkey>,
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
    fn request_start_account_monitoring(
        &self,
        pubkey: &Pubkey,
    ) -> Result<(), AccountUpdatesError> {
        self.request_sender
            .send(*pubkey)
            .map_err(AccountUpdatesError::SendError)
    }
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.last_known_update_slots
            .read()
            .unwrap()
            .get(pubkey)
            .cloned()
    }
}
