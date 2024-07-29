use std::{
    cmp::max,
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
};

use conjunto_transwise::RpcProviderConfig;
use futures_util::StreamExt;
use log::*;
use solana_account_decoder::UiDataSliceConfig;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use solana_sdk::{
    clock::Slot,
    commitment_config::{CommitmentConfig, CommitmentLevel},
    pubkey::Pubkey,
};
use thiserror::Error;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum RemoteAccountUpdatesWatcherError {
    #[error("PubsubClientError")]
    PubsubClientError(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),
    #[error("JoinError")]
    JoinError(#[from] tokio::task::JoinError),
}

pub struct RemoteAccountUpdatesWatcherRequest {
    pub account: Pubkey,
}

pub struct RemoteAccountUpdatesWatcher {
    config: RpcProviderConfig,
    last_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    request_receiver: UnboundedReceiver<RemoteAccountUpdatesWatcherRequest>,
    request_sender: UnboundedSender<RemoteAccountUpdatesWatcherRequest>,
}

impl RemoteAccountUpdatesWatcher {
    pub fn new(config: RpcProviderConfig) -> Self {
        let (request_sender, request_receiver) = mpsc::unbounded_channel();
        Self {
            config,
            last_update_slots: Default::default(),
            request_sender,
            request_receiver,
        }
    }

    pub fn get_request_sender(
        &self,
    ) -> UnboundedSender<RemoteAccountUpdatesWatcherRequest> {
        self.request_sender.clone()
    }

    pub fn get_last_update_slots(&self) -> Arc<RwLock<HashMap<Pubkey, Slot>>> {
        self.last_update_slots.clone()
    }

    pub async fn start_monitoring(
        &mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), RemoteAccountUpdatesWatcherError> {
        let pubsub_client =
            Arc::new(PubsubClient::new(self.config.ws_url()).await.map_err(
                RemoteAccountUpdatesWatcherError::PubsubClientError,
            )?);
        let commitment = self.config.commitment();

        let mut subscriptions_cancellation_tokens = HashMap::new();
        let mut subscriptions_join_handles = vec![];

        loop {
            tokio::select! {
                Some(request) = self.request_receiver.recv() => {
                    if let Entry::Vacant(entry) = subscriptions_cancellation_tokens.entry(request.account) {
                        let subscription_cancellation_token = CancellationToken::new();
                        entry.insert(subscription_cancellation_token.clone());

                        let pubsub_client = pubsub_client.clone();
                        let last_update_slots = self.last_update_slots.clone();
                        subscriptions_join_handles.push((request.account, tokio::spawn(async move {
                            let result = Self::start_monitoring_subscription(
                                last_update_slots,
                                pubsub_client,
                                commitment,
                                request.account,
                                subscription_cancellation_token,
                            ).await;
                            if let Err(error) = result {
                                warn!("Failed to monitor account: {}: {:?}", request.account, error);
                            }
                        })));
                    }
                }
                _ = cancellation_token.cancelled() => {
                    for cancellation_token in subscriptions_cancellation_tokens.into_values() {
                        cancellation_token.cancel();
                    }
                    break;
                }
            }
        }

        for (_account, handle) in subscriptions_join_handles {
            handle
                .await
                .map_err(RemoteAccountUpdatesWatcherError::JoinError)?;
        }

        Ok(())
    }

    async fn start_monitoring_subscription(
        last_update_slots: Arc<RwLock<HashMap<Pubkey, u64>>>,
        pubsub_client: Arc<PubsubClient>,
        commitment: Option<CommitmentLevel>,
        account: Pubkey,
        cancellation_token: CancellationToken,
    ) -> Result<(), RemoteAccountUpdatesWatcherError> {
        let config = Some(RpcAccountInfoConfig {
            commitment: commitment
                .map(|commitment| CommitmentConfig { commitment }),
            encoding: None,
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 0,
            }),
            min_context_slot: None,
        });

        let (mut stream, unsubscribe) = pubsub_client
            .account_subscribe(&account, config)
            .await
            .map_err(RemoteAccountUpdatesWatcherError::PubsubClientError)?;

        let cancel_handle = tokio::spawn(async move {
            cancellation_token.cancelled().await;
            unsubscribe().await;
        });

        debug!("Started monitoring updates for account: {}", account);

        while let Some(update) = stream.next().await {
            let current_update_slot = update.context.slot;
            debug!(
                "Account changed: {}, in slot: {}",
                account, current_update_slot
            );

            match last_update_slots
                .write()
                .expect("last_update_slots poisoned")
                .entry(account)
            {
                Entry::Vacant(entry) => {
                    entry.insert(current_update_slot);
                }
                Entry::Occupied(mut entry) => {
                    *entry.get_mut() = max(*entry.get(), current_update_slot);
                }
            };
        }

        debug!("Stopped monitoring updates for account: {}", account);

        cancel_handle
            .await
            .map_err(RemoteAccountUpdatesWatcherError::JoinError)
    }
}
