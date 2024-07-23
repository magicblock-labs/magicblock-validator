use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, RwLock},
};
use thiserror::Error;

use futures_util::{
    select, stream::FuturesUnordered, Future, FutureExt, Stream, StreamExt,
};
use log::*;
use solana_account_decoder::{UiAccount, UiDataSliceConfig};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    oneshot::{self, Receiver},
};

use conjunto_transwise::RpcProviderConfig;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::{config::RpcAccountInfoConfig, response::Response};
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    pubkey::Pubkey,
};

#[derive(Debug, Error)]
enum AccountUpdatesError {
    #[error("PubsubClientError")]
    PubsubClientError(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),
}

struct AccountUpdatesSubscribe {
    pub account: Pubkey,
}

struct AccountUpdatesUnsubscribe {
    pub account: Pubkey,
}

pub trait AccountUpdates {
    fn start_monitoring_account(&self, pubkey: &Pubkey);
    fn has_been_updated_since(&self, pubkey: &Pubkey, slot: u64) -> bool;
    fn stop_monitoring_account(&self, pubkey: &Pubkey);
}

pub struct RemoteAccountUpdates {
    config: RpcProviderConfig,
    last_update_slots: Arc<RwLock<HashMap<Pubkey, u64>>>,
    subscribe_receiver: UnboundedReceiver<AccountUpdatesSubscribe>,
    subscribe_sender: UnboundedSender<AccountUpdatesSubscribe>,
    unsubscribe_receiver: UnboundedReceiver<AccountUpdatesUnsubscribe>,
    unsubscribe_sender: UnboundedSender<AccountUpdatesUnsubscribe>,
}

impl AccountUpdates for RemoteAccountUpdates {
    fn start_monitoring_account(&self, pubkey: &Pubkey) {
        if let Err(error) =
            self.subscribe_sender.send(AccountUpdatesSubscribe {
                account: pubkey.clone(),
            })
        {
            error!("Failed to subscribe to account: {}: {:?}", pubkey, error)
        }
    }
    fn has_been_updated_since(&self, pubkey: &Pubkey, slot: u64) -> bool {
        let last_update_slots_read = self.last_update_slots.read().unwrap();
        if let Some(last_update_slot) = last_update_slots_read.get(pubkey) {
            *last_update_slot > slot
        } else {
            false
        }
    }
    fn stop_monitoring_account(&self, pubkey: &Pubkey) {
        if let Err(error) =
            self.unsubscribe_sender.send(AccountUpdatesUnsubscribe {
                account: pubkey.clone(),
            })
        {
            error!("Failed to unsubscribe to account: {}: {:?}", pubkey, error)
        }
    }
}

impl RemoteAccountUpdates {
    pub fn new(config: RpcProviderConfig) -> Self {
        let (subscribe_sender, subscribe_receiver) =
            unbounded_channel::<AccountUpdatesSubscribe>();
        let (unsubscribe_sender, unsubscribe_receiver) =
            unbounded_channel::<AccountUpdatesUnsubscribe>();
        Self {
            config,
            last_update_slots: Default::default(),
            subscribe_sender,
            subscribe_receiver,
            unsubscribe_sender,
            unsubscribe_receiver,
        }
    }

    async fn run(&mut self) -> Result<(), AccountUpdatesError> {
        let pubsub_client = Arc::new(
            PubsubClient::new(self.config.ws_url())
                .await
                .map_err(AccountUpdatesError::PubsubClientError)?,
        );
        let commitment = self.config.commitment();

        let last_update_slots = self.last_update_slots.clone();

        let mut cancel_senders = HashMap::new();
        let mut join_handles = vec![];

        tokio::select! {
            Some(subscribe) = self.subscribe_receiver.recv() => {
                if !cancel_senders.contains_key(&subscribe.account) {
                    let (cancel_sender, cancel_receiver) = oneshot::channel();
                    cancel_senders.insert(subscribe.account, cancel_sender);
                    join_handles.push((subscribe.account, tokio::spawn(async move {
                        let result = AccountUpdates::monitor_account(
                            last_update_slots,
                            pubsub_client,
                            commitment,
                            subscribe.account,
                            cancel_receiver,
                        ).await;
                        if let Err(error) = result {
                            warn!("Failed to monitor account: {}: {:?}", subscribe.account, error);
                        }
                    })));
                }
            }
            Some(unsubscribe) = self.unsubscribe_receiver.recv() => {
                if let Some(cancel_sender) = cancel_senders.remove(&unsubscribe.account) {
                    if let Err(error) = cancel_sender.send(()) {
                        warn!("Failed to cancel monitoring of account: {}: {:?}", unsubscribe.account, error);
                    }
                }
            }
        }

        for (account, handle) in join_handles {
            debug!("waiting on subscribe {}", account);
            if let Err(error) = handle.await {
                debug!("subscribe {} failed: {}", account, error);
            }
        }

        Ok(())
    }

    async fn monitor_account(
        last_update_slots: Arc<RwLock<HashMap<Pubkey, u64>>>,
        pubsub_client: Arc<PubsubClient>,
        commitment: Option<CommitmentLevel>,
        account: Pubkey,
        cancel_receiver: Receiver<()>,
    ) -> Result<(), AccountUpdatesError> {
        let config = Some(RpcAccountInfoConfig {
            commitment: commitment
                .map(|commitment| CommitmentConfig { commitment }),
            encoding: None,
            data_slice: None,
            /*
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 0,
            }),
             */
            min_context_slot: None,
        });

        let (mut stream, unsubscribe) = pubsub_client
            .account_subscribe(&account, config)
            .await
            .map_err(AccountUpdatesError::PubsubClientError)?;

        let cancel_handle = tokio::spawn(async move {
            match cancel_receiver.await {
                Ok(_) => unsubscribe().await,
                Err(error) => warn!(
                    "cancel_receiver failed to received unsubscribe for: {}: {:?}",
                    account, error
                ),
            };
        });

        while let Some(update) = stream.next().await {
            let current_update_slot = update.context.slot;

            let mut last_update_slots_write =
                last_update_slots.write().unwrap();
            let last_update_slot = last_update_slots_write.remove(&account);
            if let Some(last_update_slot) = last_update_slot {
                if last_update_slot >= current_update_slot {
                    continue;
                }
            }
            last_update_slots_write.insert(account, current_update_slot);
        }

        if let Err(error) = cancel_handle.await {
            warn!("cancel failed for monitoring of: {}: {:?}", account, error);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_non_existing_account() {
        /*
          let rpc_account_provider = AccountUpdates::default();
          let pubkey = Pubkey::new_from_array([5; 32]);
          let account = rpc_account_provider.get_account(&pubkey).await.unwrap();
          assert!(account.is_none());
        */
    }
}
