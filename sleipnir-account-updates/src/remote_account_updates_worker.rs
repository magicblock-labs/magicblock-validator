use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use conjunto_transwise::RpcProviderConfig;
use log::*;
use solana_sdk::{clock::Slot, pubkey::Pubkey};
use thiserror::Error;
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
    time::sleep,
};
use tokio_util::sync::CancellationToken;

use crate::RemoteAccountUpdatesShard;

#[derive(Debug, Error)]
pub enum RemoteAccountUpdatesWorkerError {
    #[error(transparent)]
    PubsubClientError(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),
}

struct RemoteAccountUpdatesWorkerRunner {
    shard: RemoteAccountUpdatesShard,
    monitoring_request_sender: UnboundedSender<Pubkey>,
    cancellation_token: CancellationToken,
    join_handle: JoinHandle<()>,
}

pub struct RemoteAccountUpdatesWorker {
    rpc_provider_config: RpcProviderConfig,
    monitoring_request_receiver: UnboundedReceiver<Pubkey>,
    monitoring_request_sender: UnboundedSender<Pubkey>,
    runners: RwLock<Vec<RemoteAccountUpdatesWorkerRunner>>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesWorker {
    pub fn new(rpc_provider_config: RpcProviderConfig) -> Self {
        let (monitoring_request_sender, monitoring_request_receiver) =
            unbounded_channel();
        Self {
            rpc_provider_config,
            monitoring_request_receiver,
            monitoring_request_sender,
            runners: Default::default(),
            last_known_update_slots: Default::default(),
        }
    }

    pub fn get_monitoring_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.monitoring_request_sender.clone()
    }

    pub fn get_last_known_update_slots(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, Slot>>> {
        self.last_known_update_slots.clone()
    }

    pub async fn start_monitoring_request_processing(
        &mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), RemoteAccountUpdatesWorkerError> {
        // Loop forever until we stop the worker
        loop {
            tokio::select! {
                // When we receive a message to start monitoring an account
                Some(pubkey) = self.monitoring_request_receiver.recv() => {
                }
                // When we want to stop the worker (it was cancelled)
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }
        // Done
        Ok(())
    }

    async fn start_monitoring_shard_auto_refresh(
        &self,
        cancellation_token: CancellationToken,
    ) {
        loop {
            tokio::select! {
                // Periodically we refresh our subscriptions shards
                _ = sleep(Duration::from_millis(10_000)) => {

                }
                // When we want to stop the worker (it was cancelled)
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }
    }
}
