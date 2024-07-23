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
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver, UnboundedSender,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum RemoteAccountUpdatesError {
    #[error("PubsubClientError")]
    PubsubClientError(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),
    #[error("JoinError")]
    JoinError(#[from] tokio::task::JoinError),
}

struct RemoteAccountUpdatesWatching {
    pub account: Pubkey,
}

pub trait AccountUpdates {
    fn ensure_monitoring_of_account(&self, pubkey: &Pubkey);
    fn has_known_update_since_slot(&self, pubkey: &Pubkey, slot: u64) -> bool;
}

pub struct RemoteAccountUpdates {
    config: RpcProviderConfig,
    last_update_slots: Arc<RwLock<HashMap<Pubkey, u64>>>,
    monitoring_receiver: UnboundedReceiver<RemoteAccountUpdatesWatching>,
    monitoring_sender: UnboundedSender<RemoteAccountUpdatesWatching>,
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

    pub async fn run(
        &mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), RemoteAccountUpdatesError> {
        let pubsub_client = Arc::new(
            PubsubClient::new(self.config.ws_url())
                .await
                .map_err(RemoteAccountUpdatesError::PubsubClientError)?,
        );
        let commitment = self.config.commitment();

        let last_update_slots = self.last_update_slots.clone();

        let mut subscriptions_cancellation_tokens = HashMap::new();
        let mut subscriptions_join_handles = vec![];

        tokio::select! {
            Some(monitoring) = self.monitoring_receiver.recv() => {
                if !subscriptions_cancellation_tokens.contains_key(&monitoring.account) {
                    let cancellation_token = CancellationToken::new();
                    subscriptions_cancellation_tokens.insert(monitoring.account, cancellation_token.clone());
                    subscriptions_join_handles.push((monitoring.account, tokio::spawn(async move {
                        let result = Self::monitor_account(
                            last_update_slots,
                            pubsub_client,
                            commitment,
                            monitoring.account,
                            cancellation_token,
                        ).await;
                        if let Err(error) = result {
                            warn!("Failed to monitor account: {}: {:?}", monitoring.account, error);
                        }
                    })));
                }
            }
            _ = cancellation_token.cancelled() => {
                for cancellation_token in subscriptions_cancellation_tokens.into_values() {
                    cancellation_token.cancel();
                }
            }
        }

        for (account, handle) in subscriptions_join_handles {
            debug!("waiting on subscribe {}", account);
            handle.await.map_err(RemoteAccountUpdatesError::JoinError)?;
        }

        Ok(())
    }

    async fn monitor_account(
        last_update_slots: Arc<RwLock<HashMap<Pubkey, u64>>>,
        pubsub_client: Arc<PubsubClient>,
        commitment: Option<CommitmentLevel>,
        account: Pubkey,
        cancellation_token: CancellationToken,
    ) -> Result<(), RemoteAccountUpdatesError> {
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
            .map_err(RemoteAccountUpdatesError::PubsubClientError)?;

        let cancel_handle = tokio::spawn(async move {
            cancellation_token.cancelled().await;
            unsubscribe().await;
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
