use std::{
    collections::{HashMap, HashSet},
    fmt,
    pin::Pin,
    sync::{
        atomic::{AtomicU16, AtomicU64, Ordering},
        Arc,
    },
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
use magicblock_metrics::metrics::{
    inc_account_subscription_account_updates_count,
    inc_program_subscription_account_updates_count,
};
use solana_account::Account;
use solana_commitment_config::CommitmentLevel as SolanaCommitmentLevel;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::clock;
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
const SUBSCRIPTION_ACTIVATION_INTERVAL_MILLIS: u64 = 10_000;
const SLOTS_BETWEEN_ACTIVATIONS: u64 =
    SUBSCRIPTION_ACTIVATION_INTERVAL_MILLIS / 400;

// -----------------
// AccountUpdateSource
// -----------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountUpdateSource {
    Account,
    Program,
}

impl fmt::Display for AccountUpdateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Account => write!(f, "account"),
            Self::Program => write!(f, "program"),
        }
    }
}

// -----------------
// ChainLaserActor
// -----------------
/// ChainLaserActor manages gRPC subscriptions to Helius Laser or Triton endpoints.
///
/// ## Subscription Lifecycle
///
/// 1. **Requested**: User calls `subscribe(pubkey)`. Pubkey is added to `subscriptions` set.
/// 2. **Queued**: Every [SUBSCRIPTION_ACTIVATION_INTERVAL_MILLIS], `update_active_subscriptions()` creates new streams.
/// 3. **Active**: Subscriptions are sent to Helius/Triton via gRPC streams in `active_subscriptions`.
/// 4. **Updates**: Account updates flow back via the streams and are forwarded to the consumer.
///
/// ## Stream Management
///
/// - Subscriptions are grouped into chunks of up to 1,000 per stream (Helius limit).
/// - Each chunk gets its own gRPC stream (`StreamMap<usize, LaserStream>`).
/// - When subscriptions change, ALL streams are dropped and recreated.
/// - This simplifies reasoning but loses in-flight updates during the transition.
///
/// ## Reconnection Behavior
///
/// - If a stream ends unexpectedly, `signal_connection_issue()` is called.
/// - The actor sends an abort signal to the submux, which triggers reconnection.
/// - The actor itself doesn't attempt to reconnect; it relies on external recovery.
pub struct ChainLaserActor {
    /// Configuration used to create the laser client
    laser_client_config: LaserstreamConfig,
    /// Requested subscriptions, some may not be active yet
    subscriptions: HashSet<Pubkey>,
    /// Subscriptions that have been activated via the helius provider
    active_subscriptions: StreamMap<usize, LaserStream>,
    /// Active streams for program subscriptions
    program_subscriptions: Option<(HashSet<Pubkey>, LaserStream)>,
    /// Receives subscribe/unsubscribe messages to this actor
    messages_receiver: mpsc::Receiver<ChainPubsubActorMessage>,
    /// Sends updates for any account subscription that is received via
    /// the Laser client subscription mechanism
    subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    /// The commitment level to use for subscriptions
    commitment: CommitmentLevel,
    /// Channel used to signal connection issues to the submux
    abort_sender: mpsc::Sender<()>,
    /// The current slot on chain, shared with RemoteAccountProvider
    chain_slot: Arc<AtomicU64>,
    /// Unique client ID including the gRPC provider name for this actor instance used in logs
    /// and metrics
    client_id: String,
}

impl ChainLaserActor {
    pub fn new_from_url(
        pubsub_url: &str,
        client_id: &str,
        api_key: &str,
        commitment: SolanaCommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        chain_slot: Arc<AtomicU64>,
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
            replay: true,
        };
        Self::new(
            client_id,
            laser_client_config,
            commitment,
            abort_sender,
            chain_slot,
        )
    }

    pub fn new(
        client_id: &str,
        laser_client_config: LaserstreamConfig,
        commitment: SolanaCommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        chain_slot: Arc<AtomicU64>,
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

        static CLIENT_ID: AtomicU16 = AtomicU16::new(0);
        // Distinguish multiple client instances of the same gRPC provider
        let client_id = format!(
            "grpc:{client_id}-{}",
            CLIENT_ID.fetch_add(1, Ordering::SeqCst)
        );
        let me = Self {
            laser_client_config,
            messages_receiver,
            subscriptions: Default::default(),
            active_subscriptions: Default::default(),
            program_subscriptions: Default::default(),
            subscription_updates_sender,
            commitment,
            abort_sender,
            chain_slot,
            client_id,
        };

        Ok((me, messages_sender, subscription_updates_receiver))
    }

    #[allow(dead_code)]
    fn shutdown(&mut self) {
        info!(
            "[client_id={}] Shutting down ChainLaserActor",
            self.client_id
        );
        self.subscriptions.clear();
        self.active_subscriptions.clear();
    }

    pub async fn run(mut self) {
        let mut activate_subs_interval =
            tokio::time::interval(std::time::Duration::from_millis(
                SUBSCRIPTION_ACTIVATION_INTERVAL_MILLIS,
            ));

        loop {
            tokio::select! {
                // Actor messages
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
                // Account subscription updates
                update = self.active_subscriptions.next(), if !self.active_subscriptions.is_empty() => {
                    match update {
                        Some(update) => {
                            self.handle_account_update(update).await;
                        }
                        None => {
                            debug!(
                                "[client_id={}] Account subscription stream ended; signaling \
                                connection issue",
                                self.client_id
                            );
                            Self::signal_connection_issue(
                                &mut self.subscriptions,
                                &mut self.active_subscriptions,
                                &mut self.program_subscriptions,
                                &self.abort_sender,
                                &self.client_id,
                            )
                            .await;
                        }
                    }
                },
                // Program subscription updates
                update = async {
                    match &mut self.program_subscriptions {
                        Some((_, stream)) => stream.next().await,
                        None => std::future::pending().await,
                    }
                }, if self.program_subscriptions.is_some() => {
                    match update {
                        Some(update) => {
                            self.handle_program_update(update).await;
                        }
                        None => {
                            debug!(
                                "[client_id={}] Program subscription stream ended; signaling \
                                connection issue",
                                self.client_id
                            );
                            Self::signal_connection_issue(
                                &mut self.subscriptions,
                                &mut self.active_subscriptions,
                                &mut self.program_subscriptions,
                                &self.abort_sender,
                                &self.client_id,
                            )
                            .await;
                        }
                    }
                },
                // Activate pending subscriptions
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
            ProgramSubscribe { pubkey, response } => {
                let commitment = self.commitment;
                let laser_client_config = self.laser_client_config.clone();
                self.add_program_sub(pubkey, commitment, laser_client_config);
                let _ = response.send(Ok(())).inspect_err(|_| {
                    warn!(
                        "[client_id={}] Failed to send program subscribe response for program {}",
                        self.client_id,
                        pubkey
                    )
                });
                false
            }
            Reconnect { response } => {
                // We cannot do much more here to _reconnect_ since we will do so once we activate
                // subscriptions again and that method does not return any error information.
                // Subscriptions were already cleared when the connection issue was signaled.
                let _ = response.send(Ok(())).inspect_err(|_| {
                    warn!(
                        "[client_id={}] Failed to send reconnect response",
                        self.client_id
                    )
                });
                false
            }
            Shutdown { response } => {
                info!(
                    "[client_id={}] Received Shutdown message",
                    self.client_id
                );
                Self::clear_subscriptions(
                    &mut self.subscriptions,
                    &mut self.active_subscriptions,
                    &mut self.program_subscriptions,
                );
                let _ = response.send(Ok(())).inspect_err(|_| {
                    warn!(
                        "[client_id={}] Failed to send shutdown response",
                        self.client_id
                    )
                });
                true
            }
        }
    }

    /// Tracks subscriptions, but does not yet activate them.
    fn add_sub(
        &mut self,
        pubkey: Pubkey,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    ) {
        if self.subscriptions.contains(&pubkey) {
            warn!(
                "[client_id={}] Already subscribed to account {}",
                self.client_id, pubkey
            );
            sub_response.send(Ok(())).unwrap_or_else(|_| {
                warn!(
                    "[client_id={}] Failed to send already subscribed response for account {}",
                    self.client_id,
                    pubkey
                )
            });
        } else {
            self.subscriptions.insert(pubkey);
            // If this is the first sub for the clock sysvar we want to activate it immediately
            if self.active_subscriptions.is_empty() {
                self.update_active_subscriptions();
            }
            sub_response.send(Ok(())).unwrap_or_else(|_| {
                warn!(
                    "[client_id={}] Failed to send subscribe response for account {}",
                    self.client_id,
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
                trace!(
                    "[client_id={}] Unsubscribed from account {}",
                    self.client_id,
                    pubkey
                );
                unsub_response.send(Ok(())).unwrap_or_else(|_| {
                    warn!(
                        "[client_id={}] Failed to send unsubscribe response for account {}",
                        self.client_id,
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
                            "[client_id={}] Failed to send unsubscribe response for account {}",
                            self.client_id,
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

        let chain_slot = self.chain_slot.load(Ordering::Relaxed);
        let from_slot = {
            if chain_slot == 0 {
                None
            } else {
                Some(chain_slot.saturating_sub(SLOTS_BETWEEN_ACTIVATIONS + 1))
            }
        };
        trace!(
            "[client_id={}] Activating {} account subs at slot {} {} using {} stream(s)",
            self.client_id,
            self.subscriptions.len(),
            chain_slot,
            from_slot.map_or("".to_string(), |s| format!("from slot: {s}")),
            chunks.len(),
        );
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let stream = Self::create_accounts_stream(
                &chunk,
                &self.commitment,
                &self.laser_client_config,
                idx,
                from_slot,
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
        _from_slot: Option<u64>,
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
            commitment: Some((*commitment).into()),
            // NOTE: triton does not support backfilling and we could not verify this with
            // helius due to being rate limited.
            // TODO(thlorenz): once we can test this enable backfilling for helius only
            from_slot: None,
            ..Default::default()
        };
        client::subscribe(laser_client_config.clone(), request).0
    }

    fn add_program_sub(
        &mut self,
        program_id: Pubkey,
        commitment: CommitmentLevel,
        laser_client_config: LaserstreamConfig,
    ) {
        let mut subscribed_programs = self
            .program_subscriptions
            .as_ref()
            .map(|x| x.0.iter().cloned().collect::<HashSet<Pubkey>>())
            .unwrap_or_default();
        subscribed_programs.insert(program_id);

        let mut accounts = HashMap::new();
        accounts.insert(
            format!("program_sub: {program_id}"),
            SubscribeRequestFilterAccounts {
                owner: subscribed_programs
                    .iter()
                    .map(|pk| pk.to_string())
                    .collect(),
                ..Default::default()
            },
        );
        let request = SubscribeRequest {
            accounts,
            commitment: Some(commitment.into()),
            ..Default::default()
        };
        let stream = client::subscribe(laser_client_config.clone(), request).0;
        self.program_subscriptions =
            Some((subscribed_programs, Box::pin(stream)));
    }

    /// Handles an update from one of the account data streams.
    async fn handle_account_update(
        &mut self,
        (idx, result): LaserStreamUpdate,
    ) {
        match result {
            Ok(subscribe_update) => {
                self.process_subscription_update(
                    subscribe_update,
                    AccountUpdateSource::Account,
                )
                .await;
            }
            Err(err) => {
                error!(
                    "[client_id={}] Error in account update stream for accounts with idx \
                     {}: {}; signaling connection issue",
                    self.client_id, idx, err
                );
                Self::signal_connection_issue(
                    &mut self.subscriptions,
                    &mut self.active_subscriptions,
                    &mut self.program_subscriptions,
                    &self.abort_sender,
                    &self.client_id,
                )
                .await;
            }
        }
    }

    /// Handles an update from the program subscriptions stream.
    async fn handle_program_update(&mut self, result: LaserResult) {
        match result {
            Ok(subscribe_update) => {
                self.process_subscription_update(
                    subscribe_update,
                    AccountUpdateSource::Program,
                )
                .await;
            }
            Err(err) => {
                error!(
                    "[client_id={}] Error in program subscription stream: {}; signaling \
                     connection issue",
                    self.client_id, err
                );
                Self::signal_connection_issue(
                    &mut self.subscriptions,
                    &mut self.active_subscriptions,
                    &mut self.program_subscriptions,
                    &self.abort_sender,
                    &self.client_id,
                )
                .await;
            }
        }
    }

    fn clear_subscriptions(
        subscriptions: &mut HashSet<Pubkey>,
        active_subscriptions: &mut StreamMap<usize, LaserStream>,
        program_subscriptions: &mut Option<(HashSet<Pubkey>, LaserStream)>,
    ) {
        subscriptions.clear();
        active_subscriptions.clear();
        *program_subscriptions = None;
    }

    /// Signals a connection issue by clearing all subscriptions and
    /// sending a message on the abort channel.
    /// NOTE: the laser client should handle reconnects internally, but
    /// we add this as a backup in case it is unable to do so
    async fn signal_connection_issue(
        subscriptions: &mut HashSet<Pubkey>,
        active_subscriptions: &mut StreamMap<usize, LaserStream>,
        program_subscriptions: &mut Option<(HashSet<Pubkey>, LaserStream)>,
        abort_sender: &mpsc::Sender<()>,
        client_id: &str,
    ) {
        debug!("[client_id={client_id}] Signaling connection issue");

        // Clear all subscriptions
        Self::clear_subscriptions(
            subscriptions,
            active_subscriptions,
            program_subscriptions,
        );

        // Use try_send to avoid blocking and naturally coalesce signals
        let _ = abort_sender.try_send(()).inspect_err(|err| {
            // Channel full is expected when reconnect is already in progress
            if !matches!(err, mpsc::error::TrySendError::Full(_)) {
                error!("[client_id={client_id}] Failed to signal connection issue: {err:?}")
            }
        });
    }

    /// Processes a subscription update from either account or program streams.
    /// We verified via a script that we get an update with Some(Account) when it is
    /// closed. In that case lamports == 0 and owner is the system program.
    /// Thus an update of `None` is not expected and can be ignored.
    /// See: https://gist.github.com/thlorenz/d3d1a380678a030b3e833f8f979319ae
    async fn process_subscription_update(
        &mut self,
        update: SubscribeUpdate,
        source: AccountUpdateSource,
    ) {
        let Some(update_oneof) = update.update_oneof else {
            return;
        };
        let UpdateOneof::Account(acc) = update_oneof else {
            return;
        };

        let (Some(account), slot) = (acc.account, acc.slot) else {
            return;
        };

        let Ok(pubkey) = Pubkey::try_from(account.pubkey) else {
            error!(
                "[client_id={}] Failed to parse pubkey in subscription update",
                self.client_id
            );
            return;
        };

        let log_trace = if log::log_enabled!(log::Level::Trace) {
            if pubkey.eq(&clock::ID) {
                static TRACE_CLOCK_COUNT: AtomicU64 = AtomicU64::new(0);
                TRACE_CLOCK_COUNT
                    .fetch_add(1, Ordering::Relaxed)
                    .is_multiple_of(100)
            } else {
                true
            }
        } else {
            false
        };
        if log_trace {
            trace!(
                "[client_id={}] Received {source} subscription update for {}: slot {}",
                self.client_id,
                pubkey,
                slot
            );
        }

        if !self.subscriptions.contains(&pubkey) {
            // Ignore updates for accounts we are not subscribed to
            return;
        }

        let Ok(owner) = Pubkey::try_from(account.owner) else {
            error!(
                "[client_id={}] Failed to parse owner pubkey in subscription update for account {}",
                self.client_id,
                pubkey
            );
            return;
        };

        let account = Account {
            lamports: account.lamports,
            data: account.data,
            owner,
            executable: account.executable,
            rent_epoch: account.rent_epoch,
        };
        let subscription_update = SubscriptionUpdate {
            pubkey,
            slot,
            account: Some(account),
        };

        if log_trace {
            trace!(
                "[client_id={}] Sending subscription update {:?} ",
                self.client_id,
                subscription_update
            );
        }

        match source {
            AccountUpdateSource::Account => {
                inc_account_subscription_account_updates_count(&self.client_id);
            }
            AccountUpdateSource::Program => {
                inc_program_subscription_account_updates_count(&self.client_id);
            }
        }

        self.subscription_updates_sender
            .send(subscription_update)
            .await
            .unwrap_or_else(|_| {
                error!(
                    "[client_id={}] Failed to send subscription update for account {}",
                    self.client_id,
                    pubkey
                )
            });
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
