use conjunto_transwise::RpcProviderConfig;
use futures_util::StreamExt;
use log::*;
use solana_account_decoder::{UiAccount, UiDataSliceConfig};
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    pubkey::Pubkey,
};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    oneshot::{self, Receiver, Sender},
};

#[derive(Debug, Error)]
enum AccountUpdatesError {
    #[error("PubsubClientError")]
    PubsubClientError(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),
}

struct AccountUpdatesMonitoring {
    pub account: Pubkey,
    pub activated: bool,
}

pub trait AccountUpdates {
    fn ensure_monitoring_of_account(&self, pubkey: &Pubkey);
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: u64) -> bool;
}

pub struct RemoteAccountUpdates {
    config: RpcProviderConfig,
    last_update_slots: Arc<RwLock<HashMap<Pubkey, u64>>>,
    monitoring_receiver: UnboundedReceiver<AccountUpdatesMonitoring>,
    monitoring_sender: UnboundedSender<AccountUpdatesMonitoring>,
}

impl AccountUpdates for RemoteAccountUpdates {
    fn ensure_monitoring_of_account(&self, pubkey: &Pubkey) {
        if let Err(error) =
            self.monitoring_sender.send(AccountUpdatesMonitoring {
                account: pubkey.clone(),
                activated: true,
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

impl RemoteAccountUpdates {
    pub fn new(config: RpcProviderConfig) -> Self {
        let (monitoring_sender, monitoring_receiver) = unbounded_channel();
        Self {
            config,
            last_update_slots: Default::default(),
            monitoring_sender,
            monitoring_receiver,
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
            Some(monitoring) = self.monitoring_receiver.recv() => {
                if !cancel_senders.contains_key(&monitoring.account) {
                    let (cancel_sender, cancel_receiver) = oneshot::channel();
                    cancel_senders.insert(monitoring.account, cancel_sender);
                    join_handles.push((monitoring.account, tokio::spawn(async move {
                        let result = Self::monitor_account(
                            last_update_slots,
                            pubsub_client,
                            commitment,
                            monitoring.account,
                            cancel_receiver,
                        ).await;
                        if let Err(error) = result {
                            warn!("Failed to monitor account: {}: {:?}", monitoring.account, error);
                        }
                    })));
                }
            }
            _ = self.close_receiver.recv() => {
                for (account, cancel_sender) in cancel_senders.into_iter() {
                    if let Err(error) = cancel_sender.send(()) {
                        warn!("Failed to stop monitoring of account: {}: {:?}", account, error);
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
