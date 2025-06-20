use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use log::*;
use solana_sdk::{
    clock::Slot, commitment_config::CommitmentLevel, pubkey::Pubkey,
};
use thiserror::Error;
use tokio::{
    sync::mpsc::{channel, Receiver, Sender},
    task::JoinHandle,
    time::interval,
};
use tokio_util::sync::CancellationToken;

use crate::RemoteAccountUpdatesShard;

const INFLIGHT_ACCOUNT_FETCHES_LIMIT: usize = 1024;

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
    monitoring_request_sender: Sender<(Pubkey, bool)>,
    cancellation_token: CancellationToken,
    join_handle: JoinHandle<()>,
}

pub struct RemoteAccountUpdatesWorker {
    ws_urls: Vec<String>,
    commitment: Option<CommitmentLevel>,
    refresh_interval: Duration,
    monitoring_request_receiver: Receiver<(Pubkey, bool)>,
    monitoring_request_sender: Sender<(Pubkey, bool)>,
    first_subscribed_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
    last_known_update_slots: Arc<RwLock<HashMap<Pubkey, Slot>>>,
}

impl RemoteAccountUpdatesWorker {
    pub fn new(
        ws_urls: Vec<String>,
        commitment: Option<CommitmentLevel>,
        refresh_interval: Duration,
    ) -> Self {
        let (monitoring_request_sender, monitoring_request_receiver) =
            channel(INFLIGHT_ACCOUNT_FETCHES_LIMIT);
        Self {
            ws_urls,
            commitment,
            refresh_interval,
            monitoring_request_receiver,
            monitoring_request_sender,
            first_subscribed_slots: Default::default(),
            last_known_update_slots: Default::default(),
        }
    }

    pub fn get_monitoring_request_sender(&self) -> Sender<(Pubkey, bool)> {
        self.monitoring_request_sender.clone()
    }

    pub fn get_first_subscribed_slots(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, Slot>>> {
        self.first_subscribed_slots.clone()
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
        for (index, url) in self.ws_urls.iter().enumerate() {
            runners.push(
                self.create_runner_from_config(
                    index,
                    url.clone(),
                    self.commitment,
                    &monitored_accounts,
                )
                .await,
            );
        }
        // Useful states
        let mut current_refresh_index = 0;
        let mut refresh_interval = interval(self.refresh_interval);
        refresh_interval.reset();
        // Loop forever until we stop the worker
        loop {
            tokio::select! {
                // When we receive a message to start monitoring an account, propagate request to all runners
                Some((pubkey, unsubscribe)) = self.monitoring_request_receiver.recv() => {
                    if monitored_accounts.contains(&pubkey) && !unsubscribe {
                        continue;
                    }
                    if !unsubscribe {
                        monitored_accounts.insert(pubkey);
                    } else {
                        monitored_accounts.remove(&pubkey);
                    }

                    for runner in runners.iter() {
                        self.notify_runner_of_monitoring_request(runner, pubkey, unsubscribe).await;
                    }
                }
                // Periodically we refresh runners to keep them fresh
                _ = refresh_interval.tick() => {
                    current_refresh_index = (current_refresh_index + 1) % self.ws_urls.len();
                    let url = self.ws_urls
                        .get(current_refresh_index)
                        .unwrap()
                        .clone();
                    let new_runner = self.create_runner_from_config(
                        current_refresh_index,
                        url,
                        self.commitment,
                        &monitored_accounts
                    ).await;
                    let old_runner = std::mem::replace(&mut runners[current_refresh_index], new_runner);
                    // We hope it ultimately joins, but we don't care to wait for it, just let it be
                    self.cancel_and_join_runner(old_runner);
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
            self.cancel_and_join_runner(runner);
        }
    }

    async fn create_runner_from_config(
        &self,
        index: usize,
        url: String,
        commitment: Option<CommitmentLevel>,
        monitored_accounts: &HashSet<Pubkey>,
    ) -> RemoteAccountUpdatesWorkerRunner {
        let (monitoring_request_sender, monitoring_request_receiver) =
            channel(INFLIGHT_ACCOUNT_FETCHES_LIMIT);
        let first_subscribed_slots = self.first_subscribed_slots.clone();
        let last_known_update_slots = self.last_known_update_slots.clone();
        let runner_id = format!("[{}:{:06}]", index, self.generate_runner_id());
        let cancellation_token = CancellationToken::new();
        let shard_id = runner_id.clone();
        let shard_cancellation_token = cancellation_token.clone();
        let join_handle = tokio::spawn(async move {
            let mut shard = RemoteAccountUpdatesShard::new(
                shard_id.clone(),
                url,
                commitment,
                monitoring_request_receiver,
                first_subscribed_slots,
                last_known_update_slots,
            );
            if let Err(error) = shard
                .start_monitoring_request_processing(shard_cancellation_token)
                .await
            {
                #[cfg(not(test))]
                error!("Runner shard has failed: {}: {:?}", shard_id, error);

                #[cfg(test)]
                panic!("Runner shard has failed: {}: {:?}", shard_id, error);
            }
        });
        let runner = RemoteAccountUpdatesWorkerRunner {
            id: runner_id,
            monitoring_request_sender,
            cancellation_token,
            join_handle,
        };
        info!("Started new runner {}", runner.id);
        for pubkey in monitored_accounts.iter() {
            self.notify_runner_of_monitoring_request(&runner, *pubkey, false)
                .await;
        }
        runner
    }

    async fn notify_runner_of_monitoring_request(
        &self,
        runner: &RemoteAccountUpdatesWorkerRunner,
        pubkey: Pubkey,
        unsubscribe: bool,
    ) {
        if let Err(error) = runner
            .monitoring_request_sender
            .send((pubkey, unsubscribe))
            .await
        {
            error!(
                "Could not send request to runner: {}: {:?}",
                runner.id, error
            );
        }
    }

    fn cancel_and_join_runner(&self, runner: RemoteAccountUpdatesWorkerRunner) {
        info!("Stopping runner {}", runner.id);
        runner.cancellation_token.cancel();
        let _join = tokio::spawn(async move {
            if let Err(error) = runner.join_handle.await {
                error!("Runner failed to shutdown: {}: {:?}", runner.id, error);
            }
        });
    }

    fn generate_runner_id(&self) -> u32 {
        static COUNTER: AtomicU32 = AtomicU32::new(1);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}
