use std::{collections::HashMap, pin::Pin};

use futures_util::{Stream, StreamExt};
use helius_laserstream::{
    client,
    grpc::{
        subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
        SubscribeRequestFilterAccounts, SubscribeUpdate,
    },
    ChannelOptions, LaserstreamConfig, LaserstreamError,
};
use log::*;
use solana_account::Account;
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamMap;

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
    subscriptions: StreamMap<Pubkey, LaserStream>,
    /// Receives subscribe/unsubscribe messages to this actor
    messages_receiver: mpsc::Receiver<ChainPubsubActorMessage>,
    /// Sends updates for any account subscription that is received via
    /// the Laser client subscription mechanism
    subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    /// The commitment level to use for subscriptions
    commitment: CommitmentLevel,
}

impl ChainLaserActor {
    pub fn new_from_url(
        pubsub_url: &str,
        api_key: &str,
        commitment: solana_sdk::commitment_config::CommitmentLevel,
    ) -> RemoteAccountProviderResult<(
        Self,
        mpsc::Sender<ChainPubsubActorMessage>,
        mpsc::Receiver<SubscriptionUpdate>,
    )> {
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
    ) -> RemoteAccountProviderResult<(
        Self,
        mpsc::Sender<ChainPubsubActorMessage>,
        mpsc::Receiver<SubscriptionUpdate>,
    )> {
        let (subscription_updates_sender, subscription_updates_receiver) =
            mpsc::channel(SUBSCRIPTION_UPDATE_CHANNEL_SIZE);
        let (messages_sender, messages_receiver) =
            mpsc::channel(MESSAGE_CHANNEL_SIZE);
        let commitment = grpc_commitment_from_solana(commitment);

        let me = Self {
            laser_client_config,
            messages_receiver,
            subscriptions: Default::default(),
            subscription_updates_sender,
            commitment,
        };

        Ok((me, messages_sender, subscription_updates_receiver))
    }

    fn shutdown(&mut self) {
        info!("Shutting down ChainLaserActor");
        self.subscriptions.clear();
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(msg) = self.messages_receiver.recv() => {
                    let is_shutdown = self.handle_msg(msg);
                    if is_shutdown {
                        break;
                    }
                }
                Some(update) = self.subscriptions.next(), if !self.subscriptions.is_empty() => {
                    self.handle_account_update(update).await;
                },
            }
        }
    }

    fn handle_msg(&mut self, msg: ChainPubsubActorMessage) -> bool {
        use ChainPubsubActorMessage::*;
        match msg {
            AccountSubscribe { pubkey, response } => {
                self.add_sub(pubkey, response);
                false
            }
            AccountUnsubscribe { pubkey, response } => {
                self.remove_sub(&pubkey, response);
                false
            }
            RecycleConnections { .. } => {
                // No-op for laserstream
                false
            }
            Shutdown { response } => {
                self.shutdown();
                response.send(Ok(())).unwrap_or_else(|_| {
                    warn!("Failed to send shutdown response")
                });
                true
            }
        }
    }

    fn add_sub(
        &mut self,
        pubkey: Pubkey,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    ) {
        let stream = self.create_account_stream(pubkey);
        if self.subscriptions.contains_key(&pubkey) {
            warn!("Already subscribed to account {}", pubkey);
        } else {
            self.subscriptions.insert(pubkey, Box::pin(stream));
        }
        sub_response.send(Ok(())).unwrap_or_else(|_| {
            warn!("Failed to send subscribe response for account {}", pubkey)
        });
    }

    fn remove_sub(
        &mut self,
        pubkey: &Pubkey,
        unsub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    ) {
        if self.subscriptions.remove(pubkey).is_some() {
            trace!("Unsubscribed from account {}", pubkey);
        }
        unsub_response.send(Ok(())).unwrap_or_else(|_| {
            warn!("Failed to send unsubscribe response for account {}", pubkey)
        });
    }

    /// Helper to create a dedicated stream for a single account.
    fn create_account_stream(
        &self,
        pubkey: Pubkey,
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
            commitment: Some(self.commitment.into()),
            ..Default::default()
        };
        client::subscribe(self.laser_client_config.clone(), request).0
    }

    /// Handles an update from one of the account data streams.
    async fn handle_account_update(
        &mut self,
        (pubkey, result): LaserStreamUpdate,
    ) {
        match result {
            Ok(SubscribeUpdate {
                update_oneof: Some(UpdateOneof::Account(acc)),
                ..
            }) => {
                // We verified via a script that we get an update with Some(Account) when it is
                // closed. In that case lamports == 0 and owner is the system program.
                // Thus an update of `None` is not expected and can be ignored.
                // See: https://gist.github.com/thlorenz/d3d1a380678a030b3e833f8f979319ae
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
                let Ok(owner) = account
                    .owner
                    .as_slice()
                    .try_into()
                    .map(Pubkey::new_from_array)
                else {
                    error!("Failed to parse owner pubkey in account update for account {}", pubkey);
                    return;
                };
                let account = Account {
                    lamports: account.lamports,
                    data: account.data,
                    owner,
                    executable: account.executable,
                    rent_epoch: account.rent_epoch,
                };
                let update = SubscriptionUpdate {
                    pubkey,
                    slot,
                    account: Some(account),
                };

                self.subscription_updates_sender
                    .send(update)
                    .await
                    .unwrap_or_else(|_| {
                        error!(
                            "Failed to send subscription update for account {}",
                            pubkey
                        )
                    });
            }
            Err(err) => {
                error!(
                    "Error in account update stream for account {}: {}",
                    pubkey, err
                );
            }
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
