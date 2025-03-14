use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use solana_sdk::{clock::Slot, pubkey::Pubkey};
use tokio::sync::mpsc::Sender;

use crate::{AccountUpdates, AccountUpdatesError, RemoteAccountUpdatesWorker};

pub struct RemoteAccountUpdatesClient {
    monitoring_request_sender: Sender<Pubkey>,
    first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesClient {
    pub fn new(worker: &RemoteAccountUpdatesWorker) -> Self {
        Self {
            monitoring_request_sender: worker.get_monitoring_request_sender(),
            first_subscribed_slots: worker.get_first_subscribed_slots(),
            last_known_update_slots: worker.get_last_known_update_slots(),
        }
    }
}

impl AccountUpdates for RemoteAccountUpdatesClient {
    async fn ensure_account_monitoring(
        &self,
        pubkey: &Pubkey,
    ) -> Result<(), AccountUpdatesError> {
        self.monitoring_request_sender
            .send(*pubkey)
            .await
            .map_err(AccountUpdatesError::SendError)
    }
    fn get_first_subscribed_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.first_subscribed_slots
            .read()
            .expect("RwLock of RemoteAccountUpdatesClient.first_subscribed_slots poisoned")
            .get(pubkey)
            .cloned()
    }
    fn get_last_known_update_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.last_known_update_slots
            .read()
            .expect("RwLock of RemoteAccountUpdatesClient.last_known_update_slots poisoned")
            .get(pubkey)
            .cloned()
    }
}
