use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
    time::Duration,
};

use conjunto_transwise::RpcProviderConfig;
use log::*;
use rand::distributions::{Alphanumeric, DistString};
use solana_sdk::{clock::Slot, pubkey::Pubkey};
use thiserror::Error;
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
    time::interval,
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
    #[error(transparent)]
    SendError(#[from] tokio::sync::mpsc::error::SendError<Pubkey>),
}

#[derive(Debug)]
struct RemoteAccountUpdatesWorkerRunner {
    id: String,
    monitoring_request_sender: UnboundedSender<Pubkey>,
    cancellation_token: CancellationToken,
    join_handle: JoinHandle<()>,
}

pub struct RemoteAccountUpdatesWorker {
    rpc_provider_configs: Vec<RpcProviderConfig>,
    refresh_interval: Duration,
    monitoring_request_receiver: UnboundedReceiver<Pubkey>,
    monitoring_request_sender: UnboundedSender<Pubkey>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesWorker {
    pub fn new(
        rpc_provider_configs: Vec<RpcProviderConfig>,
        refresh_interval: Duration,
    ) -> Self {
        let (monitoring_request_sender, monitoring_request_receiver) =
            unbounded_channel();
        Self {
            rpc_provider_configs,
            refresh_interval,
            monitoring_request_receiver,
            monitoring_request_sender,
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
    ) {
        // Maintain a runner for each config passed as parameter
        let mut runners = vec![];
        let mut monitored_accounts = HashSet::new();
        // Initialize all the runners for all configs
        for (index, rpc_provider_config) in
            self.rpc_provider_configs.iter().enumerate()
        {
            runners.push(self.create_runner_from_config(
                index,
                rpc_provider_config.clone(),
                &monitored_accounts,
            ));
        }
        // Useful states
        let mut current_refresh_index = 0;
        let mut refresh_interval = interval(self.refresh_interval);
        refresh_interval.reset();
        // Loop forever until we stop the worker
        loop {
            tokio::select! {
                // When we receive a message to start monitoring an account, propagate request to all runners
                Some(pubkey) = self.monitoring_request_receiver.recv() => {
                    if monitored_accounts.contains(&pubkey) {
                        continue;
                    }
                    monitored_accounts.insert(pubkey);
                    for runner in runners.iter() {
                        self.notify_runner_of_monitoring_request(runner, pubkey);
                    }
                }
                // Periodically we refresh runners to keep them fresh
                _ = refresh_interval.tick() => {
                    current_refresh_index = (current_refresh_index + 1) % self.rpc_provider_configs.len();
                    let rpc_provider_config = self.rpc_provider_configs
                        .get(current_refresh_index)
                        .unwrap()
                        .clone();
                    let new_runner = self.create_runner_from_config(
                        current_refresh_index,
                        rpc_provider_config,
                        &monitored_accounts
                    );
                    let old_runner = std::mem::replace(&mut runners[current_refresh_index], new_runner);
                    // We hope it ultimately joins, but we don't care to wait for it, just let it be
                    let _join = self.cancel_and_join_runner(old_runner);
                }
                // When we want to stop the worker (it was cancelled)
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }
        // Cancel all runners one by one when we are done
        while !runners.is_empty() {
            let runner = runners.swap_remove(0);
            self.cancel_and_join_runner(runner).await;
        }
    }

    fn create_runner_from_config(
        &self,
        index: usize,
        rpc_provider_config: RpcProviderConfig,
        monitored_accounts: &HashSet<Pubkey>,
    ) -> RemoteAccountUpdatesWorkerRunner {
        let (monitoring_request_sender, monitoring_request_receiver) =
            unbounded_channel();
        let last_known_update_slots = self.last_known_update_slots.clone();
        let random_name =
            Alphanumeric.sample_string(&mut rand::thread_rng(), 6);
        let id = format!("[{}:{}]", index, random_name);
        let cancellation_token = CancellationToken::new();
        let shard_id = id.clone();
        let shard_cancellation_token = cancellation_token.clone();
        let join_handle = tokio::spawn(async move {
            let mut shard = RemoteAccountUpdatesShard::new(
                shard_id.clone(),
                rpc_provider_config,
                monitoring_request_receiver,
                last_known_update_slots,
            );
            if let Err(error) = shard
                .start_monitoring_request_processing(shard_cancellation_token)
                .await
            {
                error!("Runner shard has failed: {}: {:?}", shard_id, error);
            }
        });
        let runner = RemoteAccountUpdatesWorkerRunner {
            id,
            monitoring_request_sender,
            cancellation_token,
            join_handle,
        };
        info!("Started new runner {}", runner.id);
        for pubkey in monitored_accounts.iter() {
            self.notify_runner_of_monitoring_request(&runner, *pubkey);
        }
        runner
    }

    fn notify_runner_of_monitoring_request(
        &self,
        runner: &RemoteAccountUpdatesWorkerRunner,
        pubkey: Pubkey,
    ) {
        if let Err(error) = runner.monitoring_request_sender.send(pubkey) {
            error!(
                "Could not send request to runner: {}: {:?}",
                runner.id, error
            );
        }
    }

    async fn cancel_and_join_runner(
        &self,
        runner: RemoteAccountUpdatesWorkerRunner,
    ) {
        info!("Stopping runner {}", runner.id);
        runner.cancellation_token.cancel();
        if let Err(error) = runner.join_handle.await {
            error!("Runner failed to shutdown: {}: {:?}", runner.id, error);
        }
    }
}
