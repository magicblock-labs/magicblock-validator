use std::{
    collections::{HashMap, HashSet},
    fmt,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
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
use magicblock_metrics::metrics::{
    inc_account_subscription_account_updates_count,
    inc_program_subscription_account_updates_count,
    inc_transaction_subscription_account_updates_count,
    inc_transaction_subscription_pubkey_updates_count_by,
};
use magicblock_sync::transaction_syncer;
use solana_account::Account;
use solana_commitment_config::CommitmentLevel as SolanaCommitmentLevel;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::clock;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamMap;
use tracing::*;

use crate::remote_account_provider::{
    pubsub_common::{
        ChainPubsubActorMessage, MESSAGE_CHANNEL_SIZE,
        SUBSCRIPTION_UPDATE_CHANNEL_SIZE,
    },
    rpc, ChainRpcClient, RemoteAccountProviderError,
    RemoteAccountProviderResult, SubscriptionUpdate,
};

type LaserResult = Result<SubscribeUpdate, LaserstreamError>;
type LaserStreamUpdate = (usize, LaserResult);
type LaserStream = Pin<Box<dyn Stream<Item = LaserResult> + Send>>;

const PER_STREAM_SUBSCRIPTION_LIMIT: usize = 1_000;
const SUBSCRIPTION_ACTIVATION_INTERVAL_MILLIS: u64 = 10_000;
const SLOTS_BETWEEN_ACTIVATIONS: u64 =
    SUBSCRIPTION_ACTIVATION_INTERVAL_MILLIS / 400;

// -----------------
// Slots
// -----------------
/// Shared slot tracking for activation lookback
pub struct Slots {
    /// The current slot on chain, shared with RemoteAccountProvider
    pub chain_slot: Arc<AtomicU64>,
    /// The last slot at which activation happened
    pub last_activation_slot: AtomicU64,
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
pub struct ChainLaserActor<T: ChainRpcClient> {
    /// Configuration used to create the laser client
    laser_client_config: LaserstreamConfig,
    /// Requested subscriptions, some may not be active yet
    subscriptions: HashSet<Pubkey>,
    /// Pubkeys of currently active subscriptions
    active_subscription_pubkeys: HashSet<Pubkey>,
    /// Subscriptions that have been activated via the helius provider
    active_subscriptions: StreamMap<usize, LaserStream>,
    /// Active streams for program subscriptions
    program_subscriptions: Option<(HashSet<Pubkey>, LaserStream)>,
    /// Stream for transaction updates
    transaction_stream: Option<LaserStream>,
    /// Receives subscribe/unsubscribe messages to this actor
    messages_receiver: mpsc::Receiver<ChainPubsubActorMessage>,
    /// Sends updates for any account subscription that is received via
    /// the Laser client subscription mechanism
    subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    /// The commitment level to use for subscriptions
    commitment: CommitmentLevel,
    /// Channel used to signal connection issues to the submux
    abort_sender: mpsc::Sender<()>,
    /// Slot tracking for activation lookback
    /// This is only set when the gRPC provider supports backfilling subscription updates
    slots: Option<Slots>,
    /// Unique client ID including the gRPC provider name for this actor instance used in logs
    /// and metrics
    client_id: String,
    /// RPC client for fetching account data to resolve pubkeys from Delegation program
    /// transaction updates
    rpc_client: T,
}

impl<T: ChainRpcClient> ChainLaserActor<T> {
    pub fn new_from_url(
        pubsub_url: &str,
        client_id: &str,
        api_key: &str,
        commitment: SolanaCommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        slots: Option<Slots>,
        rpc_client: T,
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
            slots,
            rpc_client,
        )
    }

    pub fn new(
        client_id: &str,
        laser_client_config: LaserstreamConfig,
        commitment: SolanaCommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        slots: Option<Slots>,
        rpc_client: T,
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
            active_subscription_pubkeys: Default::default(),
            program_subscriptions: Default::default(),
            transaction_stream: Default::default(),
            subscription_updates_sender,
            commitment,
            abort_sender,
            slots,
            client_id: client_id.to_string(),
            rpc_client,
        };

        Ok((me, messages_sender, subscription_updates_receiver))
    }

    #[allow(dead_code)]
    #[instrument(skip(self), fields(client_id = %self.client_id))]
    fn shutdown(&mut self) {
        info!("Shutting down laser actor");
        self.subscriptions.clear();
        self.active_subscriptions.clear();
        self.active_subscription_pubkeys.clear();
    }

    #[instrument(skip(self), fields(client_id = %self.client_id))]
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
                            debug!("Account subscription stream ended");
                            Self::signal_connection_issue(
                                &mut self.subscriptions,
                                &mut self.active_subscriptions,
                                &mut self.active_subscription_pubkeys,
                                &mut self.program_subscriptions,
                                &mut self.transaction_stream,
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
                            debug!("Program subscription stream ended");
                            Self::signal_connection_issue(
                                &mut self.subscriptions,
                                &mut self.active_subscriptions,
                                &mut self.active_subscription_pubkeys,
                                &mut self.program_subscriptions,
                                &mut self.transaction_stream,
                                &self.abort_sender,
                                &self.client_id,
                            )
                            .await;
                        }
                    }
                },
                // Transaction stream updates
                update = async {
                    match &mut self.transaction_stream {
                        Some(stream) => stream.next().await,
                        None => std::future::pending().await,
                    }
                }, if self.transaction_stream.is_some() => {
                    match update {
                        Some(update) => {
                            self.handle_transaction_update(update, self.rpc_client.clone()).await;
                        }
                        None => {
                            debug!("Transaction stream ended");
                            Self::signal_connection_issue(
                                &mut self.subscriptions,
                                &mut self.active_subscriptions,
                                &mut self.active_subscription_pubkeys,
                                &mut self.program_subscriptions,
                                &mut self.transaction_stream,
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
                    warn!(program_id = %pubkey, "Failed to send program subscribe response");
                });
                false
            }
            Reconnect { response } => {
                // We cannot do much more here to _reconnect_ since we will do so once we activate
                // subscriptions again and that method does not return any error information.
                // Subscriptions were already cleared when the connection issue was signaled.
                let _ = response.send(Ok(())).inspect_err(|_| {
                    warn!("Failed to send reconnect response");
                });
                false
            }
            Shutdown { response } => {
                info!("Received Shutdown message");
                Self::clear_subscriptions(
                    &mut self.subscriptions,
                    &mut self.active_subscriptions,
                    &mut self.active_subscription_pubkeys,
                    &mut self.program_subscriptions,
                    &mut self.transaction_stream,
                );
                let _ = response.send(Ok(())).inspect_err(|_| {
                    warn!("Failed to send shutdown response");
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
            warn!(pubkey = %pubkey, "Already subscribed to account");
            sub_response.send(Ok(())).unwrap_or_else(|_| {
                warn!(pubkey = %pubkey, "Failed to send already subscribed response");
            });
        } else {
            self.subscriptions.insert(pubkey);
            // If this is the first sub for the clock sysvar we want to activate it immediately
            if self.active_subscriptions.is_empty() {
                self.update_active_subscriptions();
            }
            sub_response.send(Ok(())).unwrap_or_else(|_| {
                warn!(pubkey = %pubkey, "Failed to send subscribe response");
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
                trace!(pubkey = %pubkey, "Unsubscribed from account");
                unsub_response.send(Ok(())).unwrap_or_else(|_| {
                    warn!(pubkey = %pubkey, "Failed to send unsubscribe response");
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
                        warn!(pubkey = %pubkey, "Failed to send unsubscribe response");
                    });
            }
        }
    }

    fn update_active_subscriptions(&mut self) {
        // Check if the active subscriptions match what we already have
        let new_pubkeys: HashSet<Pubkey> =
            self.subscriptions.iter().copied().collect();
        if new_pubkeys == self.active_subscription_pubkeys {
            trace!(
                count = self.subscriptions.len(),
                "Active subscriptions already up to date"
            );
            return;
        }

        let mut new_subs: StreamMap<usize, LaserStream> = StreamMap::new();

        // Re-create streams for all subscriptions
        let subs = self.subscriptions.iter().collect::<Vec<_>>();

        let chunks = subs
            .chunks(PER_STREAM_SUBSCRIPTION_LIMIT)
            .map(|chunk| chunk.to_vec())
            .collect::<Vec<_>>();

        let (chain_slot, from_slot) = self
            .determine_from_slot()
            .map(|(cs, fs)| (Some(cs), Some(fs)))
            .unwrap_or((None, None));

        if tracing::enabled!(tracing::Level::TRACE) {
            trace!(
                account_count = self.subscriptions.len(),
                chain_slot,
                from_slot,
                stream_count = chunks.len(),
                "Activating account subscriptions"
            );
        }

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
        self.active_subscription_pubkeys = new_pubkeys;
    }

    /// Determines the from_slot for backfilling subscription updates.
    ///
    /// Returns `Some((chain_slot, from_slot))` if backfilling is supported and we have a valid chain slot,
    /// otherwise returns `None`.
    fn determine_from_slot(&self) -> Option<(u64, u64)> {
        // slots is only set when backfilling is supported
        let Some(slots) = &self.slots else {
            return None;
        };

        let chain_slot = slots.chain_slot.load(Ordering::Relaxed);
        if chain_slot == 0 {
            // If we didn't get a chain slot update yet we cannot backfill
            return None;
        }

        // Get last activation slot and update to current chain slot
        let last_activation_slot = slots
            .last_activation_slot
            .swap(chain_slot, Ordering::Relaxed);

        // when this is called the first time make the best effort to find a reasonable
        // slot to backfill from.
        let from_slot = if last_activation_slot == 0 {
            chain_slot.saturating_sub(SLOTS_BETWEEN_ACTIVATIONS + 1)
        } else {
            last_activation_slot.saturating_sub(1)
        };
        Some((chain_slot, from_slot))
    }

    /// Helper to create a dedicated stream for a number of accounts.
    fn create_accounts_stream(
        pubkeys: &[&Pubkey],
        commitment: &CommitmentLevel,
        laser_client_config: &LaserstreamConfig,
        idx: usize,
        from_slot: Option<u64>,
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
            from_slot,
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
        if self
            .program_subscriptions
            .as_ref()
            .map(|(subscribed_programs, _)| {
                subscribed_programs.contains(&program_id)
            })
            .unwrap_or(false)
        {
            trace!(program_id = %program_id, "Program subscription already exists");
            return;
        }

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

    #[allow(dead_code)]
    fn setup_transaction_stream(
        &mut self,
        commitment: CommitmentLevel,
        laser_client_config: LaserstreamConfig,
    ) {
        if self.transaction_stream.is_some() {
            trace!("Transaction stream already exists");
            return;
        }

        let transactions = transaction_syncer::create_filter();
        let request = SubscribeRequest {
            transactions,
            commitment: Some(commitment.into()),
            ..Default::default()
        };
        let stream = client::subscribe(laser_client_config.clone(), request).0;
        self.transaction_stream = Some(Box::pin(stream));
    }

    /// Handles an update from one of the account data streams.
    #[instrument(skip(self), fields(client_id = %self.client_id, stream_index = %idx))]
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
                error!(error = ?err, "Error in account update stream");
                Self::signal_connection_issue(
                    &mut self.subscriptions,
                    &mut self.active_subscriptions,
                    &mut self.active_subscription_pubkeys,
                    &mut self.program_subscriptions,
                    &mut self.transaction_stream,
                    &self.abort_sender,
                    &self.client_id,
                )
                .await;
            }
        }
    }

    /// Handles an update from the program subscriptions stream.
    #[instrument(skip(self), fields(client_id = %self.client_id))]
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
                error!(error = ?err, "Error in program subscription stream");
                Self::signal_connection_issue(
                    &mut self.subscriptions,
                    &mut self.active_subscriptions,
                    &mut self.active_subscription_pubkeys,
                    &mut self.program_subscriptions,
                    &mut self.transaction_stream,
                    &self.abort_sender,
                    &self.client_id,
                )
                .await;
            }
        }
    }

    /// Handles an update from the Delegation program transaction stream.
    /// It then tries to detect undelegated accounts and fetches their latest
    /// account data via RPC before sending subscription updates.
    #[instrument(skip(self, rpc_client), fields(client_id = %self.client_id))]
    async fn handle_transaction_update(
        &mut self,
        result: LaserResult,
        rpc_client: T,
    ) {
        match result {
            Ok(subscribe_update) => match subscribe_update.update_oneof {
                Some(UpdateOneof::Transaction(tx)) => {
                    let pubkeys = transaction_syncer::process_update(&tx)
                        .map(Pubkey::from)
                        .filter(|pk| self.subscriptions.contains(pk))
                        .collect::<Vec<Pubkey>>();

                    inc_transaction_subscription_pubkey_updates_count_by(
                        pubkeys.len() as u64,
                    );

                    let slot = tx.slot;
                    let accounts_res = match rpc::fetch_accounts_with_timeout(
                        &rpc_client,
                        &pubkeys,
                        rpc_client.commitment(),
                        slot,
                        None,
                    )
                    .await
                    {
                        Ok(Ok(accounts_res)) => accounts_res,
                        Ok(Err(err)) => {
                            warn!(error = ?err, "RPC error fetching accounts for transaction update");
                            return;
                        }
                        Err(err) => {
                            warn!(error = ?err, "Timed out fetching accounts for transaction update");
                            return;
                        }
                    };

                    let context_slot = accounts_res.context.slot;
                    if context_slot < slot {
                        warn!(
                            slot = slot,
                            context_slot = context_slot,
                            "Fetched accounts context slot is less than transaction slot"
                        );
                    }
                    for (pubkey, account) in
                        pubkeys.iter().zip(accounts_res.value.into_iter())
                    {
                        if let Some(account) = account {
                            inc_transaction_subscription_account_updates_count(
                            );
                            let subscription_update = SubscriptionUpdate {
                                pubkey: *pubkey,
                                slot: context_slot,
                                account: Some(account),
                            };
                            self.subscription_updates_sender
                                .send(subscription_update)
                                .await
                                .unwrap_or_else(|_| {
                                    error!(
                                        pubkey = %pubkey,
                                        "Failed to send subscription update"
                                    );
                                });
                        } else {
                            warn!(
                                pubkey = %pubkey,
                                "Account not found when fetching its data after transaction update"
                            );
                        }
                    }
                }
                _ => {
                    warn!(
                        "Received non-transaction update on transaction stream"
                    );
                }
            },
            Err(err) => {
                error!(error = ?err, "Error in transaction stream");
                Self::signal_connection_issue(
                    &mut self.subscriptions,
                    &mut self.active_subscriptions,
                    &mut self.active_subscription_pubkeys,
                    &mut self.program_subscriptions,
                    &mut self.transaction_stream,
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
        active_subscription_pubkeys: &mut HashSet<Pubkey>,
        program_subscriptions: &mut Option<(HashSet<Pubkey>, LaserStream)>,
        transaction_stream: &mut Option<LaserStream>,
    ) {
        subscriptions.clear();
        active_subscriptions.clear();
        active_subscription_pubkeys.clear();
        *program_subscriptions = None;
        *transaction_stream = None;
    }

    /// Signals a connection issue by clearing all subscriptions and
    /// sending a message on the abort channel.
    /// NOTE: the laser client should handle reconnects internally, but
    /// we add this as a backup in case it is unable to do so
    #[instrument(skip(subscriptions, active_subscriptions, active_subscription_pubkeys, program_subscriptions, transaction_stream, abort_sender), fields(client_id = %client_id))]
    async fn signal_connection_issue(
        subscriptions: &mut HashSet<Pubkey>,
        active_subscriptions: &mut StreamMap<usize, LaserStream>,
        active_subscription_pubkeys: &mut HashSet<Pubkey>,
        program_subscriptions: &mut Option<(HashSet<Pubkey>, LaserStream)>,
        transaction_stream: &mut Option<LaserStream>,
        abort_sender: &mpsc::Sender<()>,
        client_id: &str,
    ) {
        debug!("Signaling connection issue");

        // Clear all subscriptions
        Self::clear_subscriptions(
            subscriptions,
            active_subscriptions,
            active_subscription_pubkeys,
            program_subscriptions,
            transaction_stream,
        );

        // Use try_send to avoid blocking and naturally coalesce signals
        let _ = abort_sender.try_send(()).inspect_err(|err| {
            // Channel full is expected when reconnect is already in progress
            if !matches!(err, mpsc::error::TrySendError::Full(_)) {
                error!(error = ?err, "Failed to signal connection issue");
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

        if !self.subscriptions.contains(&pubkey) {
            // Ignore updates for accounts we are not subscribed to
            return;
        }

        let Ok(owner) = Pubkey::try_from(account.owner) else {
            error!(pubkey = %pubkey, "Failed to parse owner pubkey");
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
