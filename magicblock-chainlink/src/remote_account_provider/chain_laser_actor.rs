#![allow(unused)]
use futures_util::Stream;
use log::*;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio_stream::StreamMap;

use helius_laserstream::{
    client,
    grpc::{
        subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
        SubscribeRequestFilterAccounts, SubscribeUpdate,
    },
    ChannelOptions, LaserstreamConfig, LaserstreamError,
};
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::remote_account_provider::{
    pubsub_common::{
        ChainPubsubActorMessage, MESSAGE_CHANNEL_SIZE,
        SUBSCRIPTION_UPDATE_CHANNEL_SIZE,
    },
    RemoteAccountProviderResult, SubscriptionUpdate,
};

type LaserResult = Result<SubscribeUpdate, LaserstreamError>;
type LaserStreamUpdate = (Pubkey, LaserResult);
type LaserStream = Pin<Box<dyn Stream<Item = LaserResult> + Send>>;

// -----------------
// ChainLaserActor
// -----------------
pub struct ChainLaserActor {
    /// Configuration used to create the laser client
    laser_client_config: LaserstreamConfig,
    /// Active subscriptions
    subscriptions: Arc<Mutex<StreamMap<Pubkey, LaserStream>>>,
    /// Sends subscribe/unsubscribe messages to this actor
    messages_sender: mpsc::Sender<ChainPubsubActorMessage>,
    /// Sends updates for any account subscription that is received via
    /// the [Self::pubsub_client]
    subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    /// The tasks that watch subscriptions via the [Self::pubsub_client] and
    /// channel them into the [Self::subscription_updates_sender]
    subscription_watchers: Arc<Mutex<tokio::task::JoinSet<()>>>,
    /// The token to use to cancel all subscriptions and shut down the
    /// message listener, essentially shutting down whis actor
    shutdown_token: CancellationToken,
    /// The commitment level to use for subscriptions
    commitment: CommitmentLevel,
}

impl ChainLaserActor {
    pub fn new_from_url(
        pubsub_url: &str,
        api_key: &str,
        commitment: solana_sdk::commitment_config::CommitmentLevel,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        let channel_options = ChannelOptions {
            connect_timeout_secs: Some(5),
            http2_keep_alive_interval_secs: Some(15),
            tcp_keepalive_secs: Some(30),
            ..Default::default()
        };
        let laser_client_config = LaserstreamConfig {
            api_key: api_key.to_string(),
            endpoint: pubsub_url.to_string(),
            max_reconnect_attempts: Some(4),
            channel_options,
            replay: false,
        };
        Self::new(laser_client_config, commitment)
    }

    pub fn new(
        laser_client_config: LaserstreamConfig,
        commitment: solana_sdk::commitment_config::CommitmentLevel,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        let (subscription_updates_sender, subscription_updates_receiver) =
            mpsc::channel(SUBSCRIPTION_UPDATE_CHANNEL_SIZE);
        let (messages_sender, messages_receiver) =
            mpsc::channel(MESSAGE_CHANNEL_SIZE);
        let subscription_watchers =
            Arc::new(Mutex::new(tokio::task::JoinSet::new()));
        let shutdown_token = CancellationToken::new();
        let commitment = grpc_commitment_from_solana(commitment);

        let me = Self {
            laser_client_config,
            messages_sender,
            subscriptions: Default::default(),
            subscription_updates_sender,
            subscription_watchers,
            shutdown_token,
            commitment,
        };
        me.start_worker(messages_receiver);

        Ok((me, subscription_updates_receiver))
    }

    pub async fn shutdown(&self) {
        info!("Shutting down ChainLaserActor");
        // TODO: how to shut all subs  down?
        self.shutdown_token.cancel();
    }

    fn start_worker(
        &self,
        mut messages_receiver: mpsc::Receiver<ChainPubsubActorMessage>,
    ) {
        let subs = self.subscriptions.clone();
        let subscription_watchers = self.subscription_watchers.clone();
        let shutdown_token = self.shutdown_token.clone();
        let laser_client_config = self.laser_client_config.clone();
        let commitment = self.commitment;
        let subscription_updates_sender =
            self.subscription_updates_sender.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = messages_receiver.recv() => {
                        if let Some(msg) = msg {
                            Self::handle_msg(
                                laser_client_config.clone(),
                                commitment,
                                subs.clone(),
                                subscription_watchers.clone(),
                                subscription_updates_sender.clone(),
                                msg
                            );
                        } else {
                            break;
                        }
                    }
                    _ = shutdown_token.cancelled() => {
                        break;
                    }
                }
            }
        });
    }

    fn handle_msg(
        laser_client_config: LaserstreamConfig,
        commitment: CommitmentLevel,
        subscriptions: Arc<Mutex<StreamMap<Pubkey, LaserStream>>>,
        subscription_watchers: Arc<Mutex<tokio::task::JoinSet<()>>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        msg: ChainPubsubActorMessage,
    ) -> () {
        use ChainPubsubActorMessage::*;
        match msg {
            AccountSubscribe { pubkey, response } => {
                Self::add_sub(
                    pubkey,
                    laser_client_config,
                    commitment,
                    subscriptions,
                    response,
                    subscription_watchers,
                    subscription_updates_sender,
                );
            }
            AccountUnsubscribe { pubkey, response } => {
                Self::remove_sub(&pubkey, subscriptions, response);
            }
            RecycleConnections { .. } => {
                // TODO(thlorenz): use `recycles_connections` flag to avoid this call
                trace!("No need to recycle laserstream connections");
            }
        }
    }

    fn add_sub(
        pubkey: Pubkey,
        laser_client_config: LaserstreamConfig,
        commitment: CommitmentLevel,
        subs: Arc<Mutex<StreamMap<Pubkey, LaserStream>>>,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
        subscription_watchers: Arc<Mutex<tokio::task::JoinSet<()>>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    ) {
        let stream = Self::create_account_stream(
            pubkey,
            laser_client_config,
            commitment,
        );
        let subs = &mut subs.lock().expect("subscriptions Mutex poisoned");
        if subs.contains_key(&pubkey) {
            warn!("Already subscribed to account {}", pubkey);
        } else {
            subs.insert(pubkey, Box::pin(stream));
        }
        sub_response.send(Ok(())).unwrap_or_else(|_| {
            warn!("Failed to send subscribe response for account {}", pubkey)
        });
    }

    fn remove_sub(
        pubkey: &Pubkey,
        subs: Arc<Mutex<StreamMap<Pubkey, LaserStream>>>,
        unsub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    ) {
        if subs
            .lock()
            .expect("subscriptions Mutex poisoned")
            .remove(pubkey)
            .is_some()
        {
            trace!("Unsubscribed from account {}", pubkey);
        }
        unsub_response.send(Ok(())).unwrap_or_else(|_| {
            warn!("Failed to send unsubscribe response for account {}", pubkey)
        });
    }

    /// Helper to create a dedicated stream for a single account.
    fn create_account_stream(
        pubkey: Pubkey,
        config: LaserstreamConfig,
        commitment: CommitmentLevel,
    ) -> impl Stream<Item = LaserResult> {
        let mut accounts = HashMap::new();
        accounts.insert(
            "account_subs".into(),
            SubscribeRequestFilterAccounts {
                account: vec![pubkey.to_string()],
                ..Default::default()
            },
        );
        let request = SubscribeRequest {
            accounts,
            commitment: Some(commitment.into()),
            ..Default::default()
        };
        client::subscribe(config, request).0
    }

    /// Handles an update from one of the account data streams.
    async fn handle_account_update(
        (pubkey, result): LaserStreamUpdate,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    ) {
        match result {
            Ok(SubscribeUpdate {
                update_oneof: Some(UpdateOneof::Account(acc)),
                ..
            }) => {
                let (Some(account), slot) = (acc.account, acc.slot) else {
                    return;
                };

                // Defensive check to ensure the update corresponds to the stream's key.
                if pubkey.as_ref() != account.pubkey {
                    let account_pubkey = account
                        .pubkey
                        .as_slice()
                        .try_into()
                        .map(|x| Pubkey::new_from_array(x).to_string())
                        .unwrap_or("<invalid pubkey>".to_string());
                    warn!(
                        "Received mismatched pubkey in account update stream. Expected {pubkey}, Got {account_pubkey}"
                    );
                    return;
                }
                // TODO: send sub after we normalized SubscribeUpdate to something better
                // than RpcResponse<UiAccount>
            }
            Err(err) => {}
            _ => { /* Ignore other message types */ }
        }
    }
}

// -----------------
// Helpers
// -----------------
fn grpc_commitment_from_solana(
    commitment: solana_sdk::commitment_config::CommitmentLevel,
) -> CommitmentLevel {
    use solana_sdk::commitment_config::CommitmentLevel::*;
    match commitment {
        Finalized => CommitmentLevel::Finalized,
        Confirmed => CommitmentLevel::Confirmed,
        Processed => CommitmentLevel::Processed,
    }
}
