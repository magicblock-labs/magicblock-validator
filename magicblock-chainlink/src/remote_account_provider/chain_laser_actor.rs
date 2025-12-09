use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

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
use solana_commitment_config::CommitmentLevel as SolanaCommitmentLevel;
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamMap;

use crate::remote_account_provider::{
    pubsub_common::{
        ChainPubsubActorMessage, MESSAGE_CHANNEL_SIZE,
        SUBSCRIPTION_UPDATE_CHANNEL_SIZE,
    },
    RemoteAccountProviderError, RemoteAccountProviderResult,
    SubscriptionUpdate,
};

type LaserResult = Result<SubscribeUpdate, LaserstreamError>;
type LaserStreamUpdate = (usize, LaserResult);
type LaserStream = Pin<Box<dyn Stream<Item = LaserResult> + Send>>;

const PER_STREAM_SUBSCRIPTION_LIMIT: usize = 1_000;
const SUBSCIRPTION_ACTIVATION_INTERVAL_MILLIS: u64 = 400;

// -----------------
// ChainLaserActor
// -----------------
pub struct ChainLaserActor {
    /// Configuration used to create the laser client
    laser_client_config: LaserstreamConfig,
    /// Requested subscriptions, some may not be active yet
    subscriptions: HashSet<Pubkey>,
    /// Subscriptions that have been activated via the helius provider
    active_subscriptions: StreamMap<usize, LaserStream>,
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
        commitment: SolanaCommitmentLevel,
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
        commitment: SolanaCommitmentLevel,
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
            active_subscriptions: Default::default(),
            subscription_updates_sender,
            commitment,
        };

        Ok((me, messages_sender, subscription_updates_receiver))
    }

    #[allow(dead_code)]
    fn shutdown(&mut self) {
        info!("Shutting down ChainLaserActor");
        self.subscriptions.clear();
        self.active_subscriptions.clear();
    }

    pub async fn run(mut self) {
        let mut activate_subs_interval =
            tokio::time::interval(std::time::Duration::from_millis(
                SUBSCIRPTION_ACTIVATION_INTERVAL_MILLIS,
            ));

        loop {
            tokio::select! {
                msg = self.messages_receiver.recv() => {
                    match msg {
                        Some(msg) => {
                            let is_shutdown = self.handle_msg(msg);
                            if is_shutdown {
                                break;
                            }
                        }
                        None => {
                            break;
                        }
                    }
                }
                update = self.active_subscriptions.next(), if !self.active_subscriptions.is_empty() => {
                    match update {
                        Some(update) => {
                            self.handle_account_update(update).await;
                        }
                        None => break,
                    }
                },
                _ = activate_subs_interval.tick() => {
                    self.update_active_subscriptions();
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
            ProgramSubscribe {
                pubkey: _,
                response,
            } => {
                // TODO: @@@ Laser client does not support program subscriptions yet
                let _ = response.send(Ok(()));
                false
            }
            // TODO(thlorenz): @@@ reconnect
            Reconnect { response: _ } => todo!("Handle reconnect message"),
        }
    }

    /// Tracks subscriptions, but does not yet activate them.
    fn add_sub(
        &mut self,
        pubkey: Pubkey,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    ) {
        if self.subscriptions.contains(&pubkey) {
            warn!("Already subscribed to account {}", pubkey);
            sub_response.send(Ok(())).unwrap_or_else(|_| {
                warn!(
                    "Failed to send already subscribed response for account {}",
                    pubkey
                )
            });
        } else {
            self.subscriptions.insert(pubkey);
            sub_response.send(Ok(())).unwrap_or_else(|_| {
                warn!(
                    "Failed to send subscribe response for account {}",
                    pubkey
                )
            })
        }
    }

    /// Removes a subscription, but does not yet deactivate it.
    fn remove_sub(
        &mut self,
        pubkey: &Pubkey,
        unsub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    ) {
        match self.subscriptions.remove(pubkey) {
            true => {
                trace!("Unsubscribed from account {}", pubkey);
                unsub_response.send(Ok(())).unwrap_or_else(|_| {
                    warn!(
                        "Failed to send unsubscribe response for account {}",
                        pubkey
                    )
                });
            }
            false => {
                unsub_response
                    .send(Err(
                        RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                            pubkey.to_string(),
                        ),
                    ))
                    .unwrap_or_else(|_| {
                        warn!(
                            "Failed to send unsubscribe response for account {}",
                            pubkey
                        )
                    });
            }
        }
    }

    fn update_active_subscriptions(&mut self) {
        let mut new_subs: StreamMap<usize, LaserStream> = StreamMap::new();

        // Re-create streams for all subscriptions
        let subs = self.subscriptions.iter().collect::<Vec<_>>();
        let chunks = subs
            .chunks(PER_STREAM_SUBSCRIPTION_LIMIT)
            .map(|chunk| chunk.to_vec())
            .collect::<Vec<_>>();

        for (idx, chunk) in chunks.into_iter().enumerate() {
            let stream = Self::create_accounts_stream(
                &chunk,
                &self.commitment,
                &self.laser_client_config,
                idx,
            );
            new_subs.insert(idx, Box::pin(stream));
        }

        // Drop current active subscriptions by reassignig to new ones
        self.active_subscriptions = new_subs;
    }

    /// Helper to create a dedicated stream for a number of accounts.
    fn create_accounts_stream(
        pubkeys: &[&Pubkey],
        commitment: &CommitmentLevel,
        laser_client_config: &LaserstreamConfig,
        idx: usize,
    ) -> impl Stream<Item = LaserResult> {
        let mut accounts = HashMap::new();
        accounts.insert(
            format!("account_subs: {idx}"),
            SubscribeRequestFilterAccounts {
                account: pubkeys.iter().map(|pk| pk.to_string()).collect(),
                ..Default::default()
            },
        );
        let request = SubscribeRequest {
            accounts,
            commitment: Some(commitment.clone().into()),
            ..Default::default()
        };
        client::subscribe(laser_client_config.clone(), request).0
    }

    /// Handles an update from one of the account data streams.
    async fn handle_account_update(
        &mut self,
        (idx, result): LaserStreamUpdate,
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

                let Ok(pubkey) = Pubkey::try_from(account.pubkey) else {
                    error!("Failed to parse pubkey in account update",);
                    return;
                };

                if !self.subscriptions.contains(&pubkey) {
                    // Ignore updates for accounts we are no longer subscribed to
                    return;
                }

                let Ok(owner) = Pubkey::try_from(account.owner) else {
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
                    "Error in account update stream for accounts with idx {}: {}",
                    idx, err
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
    commitment: SolanaCommitmentLevel,
) -> CommitmentLevel {
    use SolanaCommitmentLevel::*;
    match commitment {
        Finalized => CommitmentLevel::Finalized,
        Confirmed => CommitmentLevel::Confirmed,
        Processed => CommitmentLevel::Processed,
    }
}
