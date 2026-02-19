use std::{
    fmt,
    sync::{
        atomic::{AtomicU16, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use helius_laserstream::{
    grpc::{subscribe_update::UpdateOneof, CommitmentLevel, SubscribeUpdate},
    LaserstreamConfig, LaserstreamError,
};
use magicblock_core::logger::log_trace_debug;
use magicblock_metrics::metrics::{
    inc_account_subscription_account_updates_count,
    inc_per_program_account_updates_count,
    inc_program_subscription_account_updates_count,
};
use solana_account::Account;
use solana_commitment_config::CommitmentLevel as SolanaCommitmentLevel;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::clock;
use tokio::sync::{mpsc, oneshot};
use tonic::Code;
use tracing::*;

use super::{
    LaserResult, SharedSubscriptions, StreamFactory, StreamHandle,
    StreamManager, StreamManagerConfig, StreamUpdateSource,
};
use crate::remote_account_provider::{
    chain_rpc_client::{ChainRpcClient, ChainRpcClientImpl},
    chain_slot::ChainSlot,
    pubsub_common::{
        ChainPubsubActorMessage, MESSAGE_CHANNEL_SIZE,
        SUBSCRIPTION_UPDATE_CHANNEL_SIZE,
    },
    RemoteAccountProviderError, RemoteAccountProviderResult,
    SubscriptionUpdate,
};

const MAX_SLOTS_BACKFILL: u64 = 400;

// -----------------
// Slots
// -----------------
/// Shared slot tracking for activation lookback and chain slot synchronization.
#[derive(Debug)]
pub struct Slots {
    /// The current slot on chain, shared with RemoteAccountProvider.
    /// Updated via `update()` when slot updates are received from GRPC.
    /// Metrics are automatically captured on updates.
    pub chain_slot: ChainSlot,
    /// The last slot at which activation happened (used for backfilling).
    pub last_activation_slot: AtomicU64,
    /// Whether this GRPC endpoint supports backfilling subscription updates.
    pub supports_backfill: bool,
}

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
/// ChainLaserActor manages gRPC subscriptions to Helius Laser
/// or Triton endpoints.
///
/// ## Subscription Lifecycle
///
/// 1. **Requested**: User calls `subscribe(pubkey)`.
/// 2. **Active**: The pubkey is immediately forwarded to the
///    [StreamManager] which handles stream creation/chunking.
/// 3. **Updates**: Account updates flow back via the streams
///    and are forwarded to the consumer.
///
/// ## Stream Management
///
/// Stream creation, chunking, promotion, and optimization are
/// fully delegated to [StreamManager].
///
/// ## Reconnection Behavior
///
/// - If a stream ends unexpectedly, `signal_connection_issue()`
///   is called.
/// - The actor sends an abort signal to the submux, which
///   triggers reconnection.
/// - The actor itself doesn't attempt to reconnect; it relies
///   on external recovery.
pub struct ChainLaserActor<H: StreamHandle, S: StreamFactory<H>> {
    /// Manager for creating and polling laser streams
    stream_manager: StreamManager<H, S>,
    /// Receives subscribe/unsubscribe messages to this actor
    messages_receiver: mpsc::Receiver<ChainPubsubActorMessage>,
    /// Sends updates for any account subscription that is
    /// received via the Laser client subscription mechanism
    subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    /// The commitment level to use for subscriptions
    commitment: CommitmentLevel,
    /// Channel used to signal connection issues to the submux
    abort_sender: mpsc::Sender<()>,
    /// Slot tracking for chain slot synchronization and
    /// activation lookback
    slots: Slots,
    /// Unique client ID including the gRPC provider name for
    /// this actor instance used in logs and metrics
    client_id: String,
    /// RPC client for diagnostics (e.g., fetching slot when
    /// falling behind)
    rpc_client: ChainRpcClientImpl,
}

impl ChainLaserActor<super::StreamHandleImpl, super::StreamFactoryImpl> {
    pub fn new_from_url(
        pubsub_url: &str,
        client_id: &str,
        api_key: &str,
        commitment: SolanaCommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        slots: Slots,
        rpc_client: ChainRpcClientImpl,
    ) -> (
        Self,
        mpsc::Sender<ChainPubsubActorMessage>,
        mpsc::Receiver<SubscriptionUpdate>,
        SharedSubscriptions,
    ) {
        let channel_options = helius_laserstream::ChannelOptions {
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
            slots,
            rpc_client,
        )
    }

    pub fn new(
        client_id: &str,
        laser_client_config: LaserstreamConfig,
        commitment: SolanaCommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        slots: Slots,
        rpc_client: ChainRpcClientImpl,
    ) -> (
        Self,
        mpsc::Sender<ChainPubsubActorMessage>,
        mpsc::Receiver<SubscriptionUpdate>,
        SharedSubscriptions,
    ) {
        let stream_factory = super::StreamFactoryImpl::new(laser_client_config);
        Self::with_stream_factory(
            client_id,
            stream_factory,
            commitment,
            abort_sender,
            slots,
            rpc_client,
        )
    }
}

impl<H: StreamHandle, S: StreamFactory<H>> ChainLaserActor<H, S> {
    /// Create actor with a custom stream factory (for testing)
    pub fn with_stream_factory(
        client_id: &str,
        stream_factory: S,
        commitment: SolanaCommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        slots: Slots,
        rpc_client: ChainRpcClientImpl,
    ) -> (
        Self,
        mpsc::Sender<ChainPubsubActorMessage>,
        mpsc::Receiver<SubscriptionUpdate>,
        SharedSubscriptions,
    ) {
        let (subscription_updates_sender, subscription_updates_receiver) =
            mpsc::channel(SUBSCRIPTION_UPDATE_CHANNEL_SIZE);
        let (messages_sender, messages_receiver) =
            mpsc::channel(MESSAGE_CHANNEL_SIZE);
        let commitment = grpc_commitment_from_solana(commitment);

        let stream_manager = StreamManager::new(
            StreamManagerConfig::default(),
            stream_factory,
        );
        let shared_subscriptions =
            Arc::clone(stream_manager.subscriptions());

        let me = Self {
            stream_manager,
            messages_receiver,
            subscription_updates_sender,
            commitment,
            abort_sender,
            slots,
            client_id: client_id.to_string(),
            rpc_client,
        };

        (
            me,
            messages_sender,
            subscription_updates_receiver,
            shared_subscriptions,
        )
    }

    #[allow(dead_code)]
    #[instrument(skip(self), fields(client_id = %self.client_id))]
    fn shutdown(&mut self) {
        info!("Shutting down laser actor");
        Self::clear_subscriptions(&mut self.stream_manager);
    }

    #[instrument(skip(self), fields(client_id = %self.client_id))]
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                msg = self.messages_receiver.recv() => {
                    match msg {
                        Some(msg) => {
                            if self.handle_msg(msg).await {
                                break;
                            }
                        }
                        None => break,
                    }
                },
                update = self.stream_manager.next_update(), if self.stream_manager.has_any_subscriptions() => {
                    match update {
                        Some((src, result)) => {
                            self.handle_stream_result(
                                src, result,
                            ).await;
                        }
                        None => {
                            debug!(
                                "Subscription stream ended"
                            );
                            Self::signal_connection_issue(
                                &mut self.stream_manager,
                                &self.abort_sender,
                                &self.client_id,
                            )
                            .await;
                        }
                    }
                },
            }
        }
    }

    async fn handle_msg(&mut self, msg: ChainPubsubActorMessage) -> bool {
        use ChainPubsubActorMessage::*;
        match msg {
            AccountSubscribe {
                pubkey, response, ..
            } => {
                self.add_sub(pubkey, response).await;
                false
            }
            AccountUnsubscribe { pubkey, response } => {
                self.remove_sub(&pubkey, response);
                false
            }
            ProgramSubscribe { pubkey, response } => {
                let result = self
                    .stream_manager
                    .add_program_subscription(pubkey, &self.commitment)
                    .await;
                if let Err(e) = result {
                    let _ = response.send(Err(e)).inspect_err(|_| {
                        warn!(client_id = self.client_id, program_id = %pubkey, "Failed to send program subscribe response");
                    });
                } else {
                    let _ = response.send(Ok(())).inspect_err(|_| {
                        warn!(client_id = self.client_id, program_id = %pubkey, "Failed to send program subscribe response");
                    });
                };
                false
            }
            Reconnect { response } => {
                // We cannot do much more here to _reconnect_ since we will do so once we activate
                // subscriptions again and that method does not return any error information.
                // Subscriptions were already cleared when the connection issue was signaled.
                let _ = response.send(Ok(())).inspect_err(|_| {
                    warn!(
                        client_id = self.client_id,
                        "Failed to send reconnect response"
                    );
                });
                false
            }
            Shutdown { response } => {
                info!(client_id = self.client_id, "Received Shutdown message");
                Self::clear_subscriptions(
                    &mut self.stream_manager,
                );
                let _ = response.send(Ok(())).inspect_err(|_| {
                    warn!(
                        client_id = self.client_id,
                        "Failed to send shutdown response"
                    );
                });
                true
            }
        }
    }

    /// Subscribes to the given pubkey immediately by forwarding
    /// to the stream manager.
    async fn add_sub(
        &mut self,
        pubkey: Pubkey,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    ) {
        if self.stream_manager.is_subscribed(&pubkey) {
            debug!(
                pubkey = %pubkey,
                "Already subscribed to account"
            );
            sub_response.send(Ok(())).unwrap_or_else(|_| {
                warn!(pubkey = %pubkey, "Failed to send already subscribed response");
            });
            return;
        }

        let from_slot = self.determine_from_slot().map(|(_, fs)| fs);
        let result = self
            .stream_manager
            .account_subscribe(&[pubkey], &self.commitment, from_slot)
            .await;

        let response = match result {
            Ok(()) => Ok(()),
            Err(e) => {
                error!(
                    pubkey = %pubkey,
                    error = ?e,
                    "Failed to subscribe to account"
                );
                Err(e)
            }
        };
        sub_response.send(response).unwrap_or_else(|_| {
            warn!(
                pubkey = %pubkey,
                "Failed to send subscribe response"
            );
        });
    }

    /// Removes a subscription and forwards to the stream manager.
    fn remove_sub(
        &mut self,
        pubkey: &Pubkey,
        unsub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    ) {
        if self.stream_manager.is_subscribed(pubkey) {
            self.stream_manager.account_unsubscribe(&[*pubkey]);
            trace!(
                pubkey = %pubkey,
                "Unsubscribed from account"
            );
            unsub_response.send(Ok(())).unwrap_or_else(|_| {
                warn!(pubkey = %pubkey, "Failed to send unsubscribe response");
            });
        } else {
            unsub_response
                .send(Err(
                    RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                        pubkey.to_string(),
                    ),
                ))
                .unwrap_or_else(|_| {
                    warn!(pubkey = %pubkey, "Failed to send unsubscribe response");
                });
        }
    }

    /// Determines the from_slot for backfilling subscription
    /// updates.
    ///
    /// Returns `Some((chain_slot, from_slot))` if backfilling is
    /// supported and we have a valid chain slot, otherwise
    /// returns `None`.
    fn determine_from_slot(&self) -> Option<(u64, u64)> {
        if !self.slots.supports_backfill {
            return None;
        }

        let chain_slot = self.slots.chain_slot.load();
        if chain_slot == 0 {
            return None;
        }

        let last_activation_slot = self
            .slots
            .last_activation_slot
            .swap(chain_slot, Ordering::Relaxed);

        let from_slot = if last_activation_slot == 0 {
            chain_slot.saturating_sub(MAX_SLOTS_BACKFILL)
        } else {
            let target_slot = last_activation_slot.saturating_sub(1);
            let delta = chain_slot.saturating_sub(target_slot);
            if delta < MAX_SLOTS_BACKFILL {
                target_slot
            } else {
                chain_slot.saturating_sub(MAX_SLOTS_BACKFILL)
            }
        };
        Some((chain_slot, from_slot))
    }

    /// Handles an update from any subscription stream.
    #[instrument(skip(self), fields(client_id = %self.client_id))]
    async fn handle_stream_result(
        &mut self,
        src: StreamUpdateSource,
        result: LaserResult,
    ) {
        let update_source = match src {
            StreamUpdateSource::Account => AccountUpdateSource::Account,
            StreamUpdateSource::Program => AccountUpdateSource::Program,
        };
        match result {
            Ok(subscribe_update) => {
                self.process_subscription_update(
                    subscribe_update,
                    update_source,
                )
                .await;
            }
            Err(err) => {
                let label = match src {
                    StreamUpdateSource::Account => "account update",
                    StreamUpdateSource::Program => "program subscription",
                };
                self.handle_stream_error(&err, label).await;
            }
        }
    }

    /// Common error handling for stream errors. Detects "fallen
    /// behind" errors and spawns diagnostics to compare our last
    /// known slot with the actual chain slot via RPC.
    async fn handle_stream_error(
        &mut self,
        err: &LaserstreamError,
        source: &str,
    ) {
        if is_fallen_behind_error(err) {
            self.spawn_fallen_behind_diagnostics(source);
        }

        error!(
            error = ?err,
            slots = ?self.slots,
            "Error in {} stream",
            source,
        );
        Self::signal_connection_issue(
            &mut self.stream_manager,
            &self.abort_sender,
            &self.client_id,
        )
        .await;
    }

    /// Spawns an async task to fetch the current chain slot via RPC and log
    /// how far behind we were when the "fallen behind" error occurred.
    /// It also updates the current chain slot in our `chain_slot` tracker to
    /// the fetched slot if it is higher than our last known slot.
    fn spawn_fallen_behind_diagnostics(&self, source: &str) {
        let chain_slot = self.slots.chain_slot.clone();
        let last_chain_slot = chain_slot.load();
        let rpc_client = self.rpc_client.clone();
        let client_id = self.client_id.clone();
        let source = source.to_string();

        const TIMEOUT_SECS: u64 = 5;
        // At 2.5 slots per sec when we factor by 5 we allow
        // double the lag that would be caused by the max timeout alone
        const MAX_ALLOWED_LAG_SLOTS: u64 = TIMEOUT_SECS * 5;

        tokio::spawn(async move {
            let rpc_result = tokio::time::timeout(
                Duration::from_secs(TIMEOUT_SECS),
                rpc_client.get_slot(),
            )
            .await;

            match rpc_result {
                Ok(Ok(rpc_chain_slot)) => {
                    let slot_lag =
                        rpc_chain_slot.saturating_sub(last_chain_slot);
                    chain_slot.update(rpc_chain_slot);
                    if slot_lag > MAX_ALLOWED_LAG_SLOTS {
                        warn!(
                            %client_id,
                            last_chain_slot,
                            rpc_chain_slot,
                            slot_lag,
                            source,
                            "gRPC reportedly fell behind (DataLoss) due to chain_slot lagging"
                        );
                    }
                }
                Ok(Err(rpc_err)) => {
                    debug!(
                        %client_id,
                        last_chain_slot,
                        error = ?rpc_err,
                        source,
                        "Failed to fetch RPC slot for DataLoss diagnostics"
                    );
                }
                Err(_timeout) => {
                    debug!(
                        %client_id,
                        last_chain_slot,
                        source,
                        "Timeout fetching RPC slot for DataLoss diagnostics"
                    );
                }
            }
        });
    }

    fn clear_subscriptions(
        stream_manager: &mut StreamManager<H, S>,
    ) {
        stream_manager.clear_account_subscriptions();
        stream_manager.clear_program_subscriptions();
    }

    /// Signals a connection issue by clearing all subscriptions
    /// and sending a message on the abort channel.
    /// NOTE: the laser client should handle reconnects
    /// internally, but we add this as a backup in case it is
    /// unable to do so
    #[instrument(skip(stream_manager, abort_sender), fields(client_id = %client_id))]
    async fn signal_connection_issue(
        stream_manager: &mut StreamManager<H, S>,
        abort_sender: &mpsc::Sender<()>,
        client_id: &str,
    ) {
        static SIGNAL_CONNECTION_COUNT: AtomicU16 = AtomicU16::new(0);
        log_trace_debug(
            "Signaling connection issue",
            "Signaled connection issue",
            &client_id,
            &RemoteAccountProviderError::ConnectionDisrupted,
            100,
            &SIGNAL_CONNECTION_COUNT,
        );

        Self::clear_subscriptions(stream_manager);

        // Use try_send to avoid blocking and naturally
        // coalesce signals
        let _ = abort_sender.try_send(()).inspect_err(|err| {
            if !matches!(err, mpsc::error::TrySendError::Full(_)) {
                error!(
                    error = ?err,
                    "Failed to signal connection issue"
                );
            }
        });
    }

    /// Processes a subscription update from either account or program streams.
    /// We verified via a script that we get an update with Some(Account) when it is
    /// closed. In that case lamports == 0 and owner is the system program.
    /// Thus an update of `None` is not expected and can be ignored.
    /// See: https://gist.github.com/thlorenz/d3d1a380678a030b3e833f8f979319ae
    #[instrument(
        skip(self, update),
        fields(
            client_id = %self.client_id,
            pubkey = tracing::field::Empty,
            slot = tracing::field::Empty,
            source = %source,
        )
    )]
    async fn process_subscription_update(
        &mut self,
        update: SubscribeUpdate,
        source: AccountUpdateSource,
    ) {
        let Some(update_oneof) = update.update_oneof else {
            return;
        };

        // Handle slot updates - update chain_slot to max of current and received
        if let UpdateOneof::Slot(slot_update) = &update_oneof {
            self.slots.chain_slot.update(slot_update.slot);
            return;
        }

        let UpdateOneof::Account(acc) = update_oneof else {
            return;
        };

        let (Some(account), slot) = (acc.account, acc.slot) else {
            return;
        };

        let Ok(pubkey) = Pubkey::try_from(account.pubkey) else {
            error!("Failed to parse pubkey");
            return;
        };

        tracing::Span::current()
            .record("pubkey", tracing::field::display(pubkey));

        let log_trace = if tracing::enabled!(tracing::Level::TRACE) {
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

        tracing::Span::current().record("slot", slot);

        if log_trace {
            trace!("Received subscription update");
        }

        let Ok(owner) = Pubkey::try_from(account.owner) else {
            error!(pubkey = %pubkey, "Failed to parse owner pubkey");
            return;
        };

        if matches!(source, AccountUpdateSource::Program) {
            inc_per_program_account_updates_count(
                &self.client_id,
                &owner.to_string(),
            );
        }

        if !self.stream_manager.is_subscribed(&pubkey) {
            return;
        }

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

        if pubkey != clock::ID {
            match source {
                AccountUpdateSource::Account => {
                    inc_account_subscription_account_updates_count(
                        &self.client_id,
                    );
                }
                AccountUpdateSource::Program => {
                    inc_program_subscription_account_updates_count(
                        &self.client_id,
                    );
                }
            }
        }

        self.subscription_updates_sender
            .send(subscription_update)
            .await
            .unwrap_or_else(|_| {
                error!(pubkey = %pubkey, "Failed to send subscription update");
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

/// Detects if a LaserstreamError indicates the client has fallen behind the
/// stream and cannot catch up. This occurs when the client cannot consume
/// messages fast enough and falls more than 500 slots behind.
fn is_fallen_behind_error(err: &LaserstreamError) -> bool {
    match err {
        LaserstreamError::Status(status) => {
            status.code() == Code::DataLoss
                && status
                    .message()
                    .to_ascii_lowercase()
                    .contains("fallen behind")
        }
        _ => false,
    }
}
