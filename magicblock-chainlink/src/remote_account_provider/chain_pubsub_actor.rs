use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc, Mutex,
    },
};

use futures_util::stream::FuturesUnordered;
use magicblock_core::logger::{log_trace_debug, log_trace_warn};
use magicblock_metrics::metrics::{
    inc_account_subscription_account_updates_count,
    inc_per_program_account_updates_count,
    inc_program_subscription_account_updates_count,
};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_pubsub_client::pubsub_client::PubsubClientError;
use solana_rpc_client_api::{
    config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    response::Response as RpcResponse,
};
use solana_sysvar::clock;
use tokio::{
    sync::{mpsc, oneshot},
    time::Duration,
};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::*;

use super::{
    errors::{RemoteAccountProviderError, RemoteAccountProviderResult},
    pubsub_connection_pool::PubSubConnectionPool,
};
use crate::remote_account_provider::{
    pubsub_common::{
        AccountSubscription, ChainPubsubActorMessage, PubsubClientConfig,
        SubscriptionUpdate, MESSAGE_CHANNEL_SIZE,
        SUBSCRIPTION_UPDATE_CHANNEL_SIZE,
    },
    pubsub_connection::{
        ProgramSubscribeResult, PubsubConnectionImpl, SubscribeResult,
    },
    DEFAULT_SUBSCRIPTION_RETRIES,
};

// Log every 10 secs (given chain slot time is 400ms)
const CLOCK_LOG_SLOT_FREQ: u64 = 25;

// -----------------
// ChainPubsubActor
// -----------------
pub struct ChainPubsubActor {
    /// Configuration used to create the pubsub client
    pubsub_client_config: PubsubClientConfig,
    /// Underlying pubsub connection pool to connect to the chain
    pubsub_connection: Arc<PubSubConnectionPool<PubsubConnectionImpl>>,
    /// Sends subscribe/unsubscribe messages to this actor
    messages_sender: mpsc::Sender<ChainPubsubActorMessage>,
    /// Map of subscriptions we are holding
    subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
    /// Map of program account subscriptions we are holding
    program_subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
    /// Sends updates for any account subscription that is received via
    /// the [Self::pubsub_connection]
    subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    /// The token to use to cancel all subscriptions and shut down the
    /// message listener, essentially shutting down whis actor
    shutdown_token: CancellationToken,
    /// Unique client ID including the RPC provider name for this actor instance used in logs
    /// and metrics
    client_id: String,
    /// Indicates whether the actor is connected or has been disconnected due RPC to connection
    /// issues
    is_connected: Arc<AtomicBool>,
    /// Channel used to signal connection issues to the submux
    abort_sender: mpsc::Sender<()>,
}

impl ChainPubsubActor {
    pub async fn new_from_url(
        pubsub_url: &str,
        client_id: &str,
        abort_sender: mpsc::Sender<()>,
        commitment: CommitmentConfig,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        let config = PubsubClientConfig::from_url(pubsub_url, commitment);
        Self::new(client_id, abort_sender, config).await
    }

    pub async fn new(
        client_id: &str,
        abort_sender: mpsc::Sender<()>,
        pubsub_client_config: PubsubClientConfig,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        let url = pubsub_client_config.pubsub_url.clone();
        let limit = pubsub_client_config
            .per_stream_subscription_limit
            .unwrap_or(usize::MAX);
        let pubsub_connection = Arc::new(
            PubSubConnectionPool::new(url, limit, client_id.to_string())
                .await
                .inspect_err(|err| {
                    error!(
                        client_id = client_id,
                        err = ?err,
                        "Failed to connect to provider"
                    )
                })?,
        );
        Self::new_with_pool(
            client_id,
            abort_sender,
            pubsub_client_config,
            pubsub_connection,
        )
        .await
    }

    pub async fn new_with_pool(
        client_id: &str,
        abort_sender: mpsc::Sender<()>,
        pubsub_client_config: PubsubClientConfig,
        pubsub_connection: Arc<PubSubConnectionPool<PubsubConnectionImpl>>,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        let (subscription_updates_sender, subscription_updates_receiver) =
            mpsc::channel(SUBSCRIPTION_UPDATE_CHANNEL_SIZE);
        let (messages_sender, messages_receiver) =
            mpsc::channel(MESSAGE_CHANNEL_SIZE);

        let shutdown_token = CancellationToken::new();
        let me = Self {
            pubsub_client_config,
            pubsub_connection,
            messages_sender,
            subscriptions: Default::default(),
            program_subs: Default::default(),
            subscription_updates_sender,
            shutdown_token,
            client_id: client_id.to_string(),
            is_connected: Arc::new(AtomicBool::new(true)),
            abort_sender,
        };
        me.start_worker(messages_receiver);

        // Listened on by the client of this actor to receive updates for
        // subscribed accounts
        Ok((me, subscription_updates_receiver))
    }

    #[instrument(skip(subscriptions, program_subs, shutdown_token), fields(client_id = %client_id))]
    async fn shutdown(
        client_id: &str,
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        program_subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        shutdown_token: CancellationToken,
    ) {
        info!(client_id = client_id, "Shutting down pubsub actor");
        Self::unsubscribe_all(subscriptions, program_subs);
        shutdown_token.cancel();
    }

    fn unsubscribe_all(
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        program_subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
    ) {
        let subs = subscriptions
            .lock()
            .expect("subscriptions lock poisoned")
            .drain()
            .chain(
                program_subs
                    .lock()
                    .expect("program subs lock poisoned")
                    .drain(),
            )
            .collect::<Vec<_>>();
        for (_, sub) in subs {
            sub.cancellation_token.cancel();
        }
    }

    pub fn subscriptions(&self) -> HashSet<Pubkey> {
        if !self.is_connected.load(Ordering::SeqCst) {
            return HashSet::new();
        }
        let subs = self
            .subscriptions
            .lock()
            .expect("subscriptions lock poisoned");
        subs.keys().copied().collect()
    }

    pub async fn send_msg(
        &self,
        msg: ChainPubsubActorMessage,
    ) -> RemoteAccountProviderResult<()> {
        self.messages_sender.send(msg).await.map_err(|err| {
            RemoteAccountProviderError::ChainPubsubActorSendError(
                err.to_string(),
                format!("{err:#?}"),
            )
        })
    }

    fn start_worker(
        &self,
        mut messages_receiver: mpsc::Receiver<ChainPubsubActorMessage>,
    ) {
        let subs = self.subscriptions.clone();
        let program_subs = self.program_subs.clone();
        let shutdown_token = self.shutdown_token.clone();
        let pubsub_client_config = self.pubsub_client_config.clone();
        let subscription_updates_sender =
            self.subscription_updates_sender.clone();
        let pubsub_connection = self.pubsub_connection.clone();
        let client_id = self.client_id.clone();
        let is_connected = self.is_connected.clone();
        let abort_sender = self.abort_sender.clone();
        tokio::spawn(async move {
            let mut pending_messages = FuturesUnordered::new();
            loop {
                tokio::select! {
                    msg = messages_receiver.recv() => {
                        if let Some(msg) = msg {
                            let subs = subs.clone();
                            let program_subs = program_subs.clone();
                            let pubsub_connection = pubsub_connection.clone();
                            let subscription_updates_sender = subscription_updates_sender.clone();
                            let pubsub_client_config = pubsub_client_config.clone();
                            let abort_sender = abort_sender.clone();
                            let is_connected = is_connected.clone();
                            pending_messages.push(Self::handle_msg(
                                subs,
                                program_subs,
                                pubsub_connection,
                                subscription_updates_sender,
                                pubsub_client_config,
                                abort_sender,
                                &client_id,
                                is_connected,
                                shutdown_token.clone(),
                                msg
                            ));
                        } else {
                            break;
                        }
                    }
                    _ = pending_messages.next(), if !pending_messages.is_empty() => {}
                    _ = shutdown_token.cancelled() => {
                        break;
                    }
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(subscriptions, program_subs, pubsub_connection, subscription_updates_sender, pubsub_client_config, abort_sender, is_connected, shutdown_token, msg), fields(client_id = %client_id))]
    async fn handle_msg(
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        program_subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        pubsub_connection: Arc<PubSubConnectionPool<PubsubConnectionImpl>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        pubsub_client_config: PubsubClientConfig,
        abort_sender: mpsc::Sender<()>,
        client_id: &str,
        is_connected: Arc<AtomicBool>,
        shutdown_token: CancellationToken,
        msg: ChainPubsubActorMessage,
    ) {
        fn send_ok(
            response: oneshot::Sender<RemoteAccountProviderResult<()>>,
            client_id: &str,
        ) {
            let _ = response.send(Ok(())).inspect_err(|err| {
                warn!(error = ?err, client_id = %client_id, "Failed to send msg ack");
            });
        }

        match msg {
            ChainPubsubActorMessage::AccountSubscribe {
                pubkey,
                retries,
                response,
            } => {
                if !is_connected.load(Ordering::SeqCst) {
                    static SUBSCRIPTION_DURING_DISCONNECT_COUNT: AtomicU16 =
                        AtomicU16::new(0);
                    let err =
                        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                            format!("Client {client_id} disconnected"),
                        );
                    log_trace_warn(
                        "Ignoring subscribe request because disconnected",
                        "Ignored subscribe requests because disconnected",
                        &pubkey,
                        &err,
                        1000,
                        &SUBSCRIPTION_DURING_DISCONNECT_COUNT,
                    );

                    let _ = response.send(Err(err));
                    return;
                }
                let commitment_config = pubsub_client_config.commitment_config;
                Self::add_sub(
                    pubkey,
                    response,
                    subscriptions,
                    program_subs,
                    pubsub_connection,
                    subscription_updates_sender,
                    abort_sender,
                    is_connected,
                    commitment_config,
                    retries,
                    client_id,
                )
                .await;
            }
            ChainPubsubActorMessage::AccountUnsubscribe {
                pubkey,
                response,
            } => {
                if !is_connected.load(Ordering::SeqCst) {
                    static UNSUBSCRIPTION_DURING_DISCONNECT_COUNT: AtomicU16 =
                        AtomicU16::new(0);
                    log_trace_warn(
                        "Ignoring unsubscribe request because disconnected",
                        "Ignored unsubscribe requests because disconnected",
                        &pubkey,
                        &"Client disconnected",
                        1000,
                        &UNSUBSCRIPTION_DURING_DISCONNECT_COUNT,
                    );
                    send_ok(response, client_id);
                    return;
                }
                if let Some(AccountSubscription { cancellation_token }) =
                    subscriptions
                        .lock()
                        .expect("subcriptions lock poisoned")
                        .get(&pubkey)
                {
                    cancellation_token.cancel();
                    send_ok(response, client_id);
                } else {
                    let _ = response
                        .send(Err(RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                            pubkey.to_string(),
                        )));
                }
            }
            ChainPubsubActorMessage::ProgramSubscribe { pubkey, response } => {
                if !is_connected.load(Ordering::SeqCst) {
                    static SUBSCRIPTION_DURING_DISCONNECT_COUNT: AtomicU16 =
                        AtomicU16::new(0);
                    let err =
                        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                            format!("Client {client_id} disconnected"),
                        );
                    log_trace_warn(
                        "Ignoring program subscribe request because disconnected",
                        "Ignored program subscribe requests because disconnected",
                        &pubkey,
                        &"Client disconnected",
                        1000,
                        &SUBSCRIPTION_DURING_DISCONNECT_COUNT,
                    );
                    let _ = response.send(Err(err));
                    return;
                }
                let commitment_config = pubsub_client_config.commitment_config;
                Self::add_program_sub(
                    pubkey,
                    response,
                    subscriptions,
                    program_subs,
                    pubsub_connection,
                    subscription_updates_sender,
                    abort_sender,
                    is_connected,
                    commitment_config,
                    client_id,
                )
                .await;
            }
            ChainPubsubActorMessage::Reconnect { response } => {
                let result = Self::try_reconnect(
                    pubsub_connection,
                    pubsub_client_config,
                    subscriptions,
                    program_subs,
                    client_id,
                    is_connected,
                )
                .await;
                let _ = response.send(result);
            }
            ChainPubsubActorMessage::Shutdown { response } => {
                Self::shutdown(
                    client_id,
                    subscriptions,
                    program_subs,
                    shutdown_token,
                )
                .await;
                let _ = response.send(Ok(()));
            }
        }
    }

    fn is_connection_level_error(err: &PubsubClientError) -> bool {
        PubSubConnectionPool::<PubsubConnectionImpl>::is_connection_level_error(
            err,
        )
    }

    async fn subscribe_account_with_retry(
        pubsub_connection: &PubSubConnectionPool<PubsubConnectionImpl>,
        pubkey: Pubkey,
        config: RpcAccountInfoConfig,
        retries: usize,
    ) -> SubscribeResult {
        let mut retries_left = retries;
        let initial_tries = retries;

        loop {
            match pubsub_connection
                .account_subscribe(&pubkey, config.clone())
                .await
            {
                Ok(res) => return Ok(res),
                Err(err) => {
                    if retries_left == 0 {
                        return Err(err);
                    }
                    retries_left -= 1;
                    let backoff_ms =
                        50u64 * (initial_tries - retries_left) as u64;
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }

    async fn subscribe_program_with_retry(
        pubsub_connection: &PubSubConnectionPool<PubsubConnectionImpl>,
        program_pubkey: Pubkey,
        config: RpcProgramAccountsConfig,
        retries: usize,
    ) -> ProgramSubscribeResult {
        let mut retries_left = retries;
        let initial_tries = retries;

        loop {
            match pubsub_connection
                .program_subscribe(&program_pubkey, config.clone())
                .await
            {
                Ok(res) => return Ok(res),
                Err(err) => {
                    if retries_left == 0 {
                        return Err(err);
                    }
                    retries_left -= 1;
                    let backoff_ms =
                        50u64 * (initial_tries - retries_left) as u64;
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(sub_response, subs, program_subs, pubsub_connection, subscription_updates_sender, abort_sender, is_connected), fields(client_id = %client_id, pubkey = %pubkey, commitment = ?commitment_config))]
    async fn add_sub(
        pubkey: Pubkey,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
        subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        program_subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        pubsub_connection: Arc<PubSubConnectionPool<PubsubConnectionImpl>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        abort_sender: mpsc::Sender<()>,
        is_connected: Arc<AtomicBool>,
        commitment_config: CommitmentConfig,
        retries: Option<usize>,
        client_id: &str,
    ) {
        if subs
            .lock()
            .expect("subscriptions lock poisoned")
            .contains_key(&pubkey)
        {
            trace!("Subscription already exists");
            let _ = sub_response.send(Ok(()));
            return;
        }

        trace!("Adding subscription");

        let cancellation_token = CancellationToken::new();

        // Insert into subscriptions HashMap immediately to prevent race condition
        // with unsubscribe operations
        // Assuming that messages to this actor are processed in the order they are sent
        // then this eliminates the possibility of an unsubscribe being processed before
        // the sub's cancellation token was added to the map
        {
            let mut subs_lock =
                subs.lock().expect("subscriptions lock poisoned");
            subs_lock.insert(
                pubkey,
                AccountSubscription {
                    cancellation_token: cancellation_token.clone(),
                },
            );
        }

        let config = RpcAccountInfoConfig {
            commitment: Some(commitment_config),
            encoding: Some(UiAccountEncoding::Base64Zstd),
            ..Default::default()
        };

        let retries = retries.unwrap_or(DEFAULT_SUBSCRIPTION_RETRIES);
        let (mut update_stream, initial_unsubscribe) =
            match Self::subscribe_account_with_retry(
                pubsub_connection.as_ref(),
                pubkey,
                config.clone(),
                retries,
            )
            .await
            {
                Ok(res) => res,
                Err(err) => {
                    if retries > 0 {
                        warn!(
                            error = ?err,
                            pubkey = %pubkey,
                            retries_count = retries,
                            "Failed to subscribe to account after retrying multiple times",
                        );
                    }

                    subs.lock()
                        .expect("subscriptions lock poisoned")
                        .remove(&pubkey);
                    if Self::is_connection_level_error(&err) {
                        Self::abort_and_signal_connection_issue(
                            client_id,
                            subs.clone(),
                            program_subs.clone(),
                            abort_sender,
                            is_connected.clone(),
                            &format!("Failed to subscribe to account {pubkey} after {retries} retries"),
                        );
                    }
                    // RPC failed - inform the requester
                    let _ = sub_response.send(Err(err.into()));
                    return;
                }
            };

        // RPC succeeded - confirm to the requester that the subscription was made
        let _ = sub_response.send(Ok(()));

        let client_id = client_id.to_string();
        let pubsub_connection = pubsub_connection.clone();
        tokio::spawn(async move {
            let mut unsubscribe = Some(initial_unsubscribe);
            // Now keep listening for updates and relay them to the
            // subscription updates sender until it is cancelled
            loop {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        trace!("Subscription was cancelled");
                        break;
                    }
                    update = update_stream.next() => {
                        if let Some(rpc_response) = update {
                            if tracing::enabled!(tracing::Level::TRACE) && (!pubkey.eq(&clock::ID) ||
                                rpc_response.context.slot % CLOCK_LOG_SLOT_FREQ == 0) {
                                trace!(slot = rpc_response.context.slot, "Received subscription update");
                            }
                            let update = SubscriptionUpdate::from((pubkey, rpc_response));
                            if pubkey != clock::ID {
                                inc_account_subscription_account_updates_count(
                                    &client_id,
                                );
                            }
                            let _ = subscription_updates_sender.send(update).await.inspect_err(|err| {
                                error!(error = ?err, "Failed to send subscription update");
                            });
                        } else {
                            if cancellation_token.is_cancelled() {
                                break;
                            }
                            static SIGNAL_CONNECTION_COUNT: AtomicU16 =
                                AtomicU16::new(0);
                            log_trace_debug(
                                "Subscription ended; attempting connection-local recovery",
                                "Subscriptions ended; attempted connection-local recovery",
                                &client_id,
                                &RemoteAccountProviderError::ConnectionDisrupted,
                                100,
                                &SIGNAL_CONNECTION_COUNT,
                            );
                            if let Some(unsub) = unsubscribe.take() {
                                if tokio::time::timeout(Duration::from_secs(2), unsub())
                                    .await
                                    .is_err()
                                {
                                    warn!(timeout_ms = 2000, "Unsubscribe timed out while recovering account subscription");
                                }
                            }
                            match Self::subscribe_account_with_retry(
                                pubsub_connection.as_ref(),
                                pubkey,
                                config.clone(),
                                retries,
                            )
                            .await
                            {
                                Ok((new_stream, new_unsub)) => {
                                    update_stream = new_stream;
                                    unsubscribe = Some(new_unsub);
                                    continue;
                                }
                                Err(err) => {
                                    warn!(
                                        client_id = %client_id,
                                        pubkey = %pubkey,
                                        retries_count = retries,
                                        error = ?err,
                                        "Failed to recover dropped account subscription",
                                    );
                                    if Self::is_connection_level_error(&err) {
                                        Self::abort_and_signal_connection_issue(
                                            &client_id,
                                            subs.clone(),
                                            program_subs.clone(),
                                            abort_sender.clone(),
                                            is_connected.clone(),
                                            &format!(
                                                "Failed to recover dropped account subscription for {pubkey} after {retries} retries"
                                            ),
                                        );
                                        return;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            // Clean up subscription with timeout to prevent hanging on dead sockets
            if let Some(unsub) = unsubscribe.take() {
                if tokio::time::timeout(Duration::from_secs(2), unsub())
                    .await
                    .is_err()
                {
                    warn!(timeout_ms = 2000, "Unsubscribe timed out");
                }
            }
            subs.lock()
                .expect("subscriptions lock poisoned")
                .remove(&pubkey);
        });
    }
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(sub_response, subs, program_subs, pubsub_connection, subscription_updates_sender, abort_sender, is_connected), fields(client_id = %client_id, program_id = %program_pubkey, commitment = ?commitment_config))]
    async fn add_program_sub(
        program_pubkey: Pubkey,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
        subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        program_subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        pubsub_connection: Arc<PubSubConnectionPool<PubsubConnectionImpl>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        abort_sender: mpsc::Sender<()>,
        is_connected: Arc<AtomicBool>,
        commitment_config: CommitmentConfig,
        client_id: &str,
    ) {
        if program_subs
            .lock()
            .expect("program subscriptions lock poisoned")
            .contains_key(&program_pubkey)
        {
            trace!("Program subscription already exists");
            let _ = sub_response.send(Ok(()));
            return;
        }

        trace!("Adding program subscription");

        let cancellation_token = CancellationToken::new();

        {
            let mut program_subs_lock = program_subs
                .lock()
                .expect("program subscriptions lock poisoned");
            program_subs_lock.insert(
                program_pubkey,
                AccountSubscription {
                    cancellation_token: cancellation_token.clone(),
                },
            );
        }

        let config = RpcProgramAccountsConfig {
            account_config: RpcAccountInfoConfig {
                commitment: Some(commitment_config),
                encoding: Some(UiAccountEncoding::Base64Zstd),
                ..Default::default()
            },
            ..Default::default()
        };

        let retries = DEFAULT_SUBSCRIPTION_RETRIES;
        let (mut update_stream, initial_unsubscribe) =
            match Self::subscribe_program_with_retry(
                pubsub_connection.as_ref(),
                program_pubkey,
                config.clone(),
                retries,
            )
            .await
            {
                Ok(res) => res,
                Err(err) => {
                    static SUBSCRIPTION_FAILURE_COUNT: AtomicU16 =
                        AtomicU16::new(0);
                    log_trace_warn(
                        "Failed to subscribe to program",
                        "Failed to subscribe to programs",
                        &program_pubkey,
                        &err,
                        100,
                        &SUBSCRIPTION_FAILURE_COUNT,
                    );
                    program_subs
                        .lock()
                        .expect("program subscriptions lock poisoned")
                        .remove(&program_pubkey);
                    if Self::is_connection_level_error(&err) {
                        Self::abort_and_signal_connection_issue(
                            client_id,
                            subs.clone(),
                            program_subs.clone(),
                            abort_sender,
                            is_connected.clone(),
                            &format!(
                                "Failed to subscribe to program {program_pubkey} after {retries} retries"
                            ),
                        );
                    }
                    // RPC failed - inform the requester
                    let _ = sub_response.send(Err(err.into()));
                    return;
                }
            };

        // RPC succeeded - confirm to the requester that the subscription was made
        let _ = sub_response.send(Ok(()));

        let client_id = client_id.to_string();
        let pubsub_connection = pubsub_connection.clone();
        tokio::spawn(async move {
            let mut unsubscribe = Some(initial_unsubscribe);
            // Now keep listening for updates and relay matching accounts to the
            // subscription updates sender until it is cancelled
            loop {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        trace!("Program subscription was cancelled");
                        break;
                    }
                    update = update_stream.next() => {
                        if let Some(rpc_response) = update {
                            let acc_pubkey = rpc_response.value.pubkey
                                .parse::<Pubkey>().inspect_err(|err| {
                                    warn!(error = ?err, pubkey_string = %rpc_response.value.pubkey, "Received invalid pubkey in program subscription update");
                                });
                            if let Ok(acc_pubkey) = acc_pubkey {
                                inc_per_program_account_updates_count(
                                    &client_id,
                                    &program_pubkey.to_string(),
                                );

                                if subs.lock().expect("subscriptions lock poisoned").contains_key(&acc_pubkey) {
                                    let ui_account = rpc_response.value.account;
                                    let rpc_response = RpcResponse {
                                        context: rpc_response.context,
                                        value: ui_account,
                                    };
                                    let sub_update = SubscriptionUpdate::from((acc_pubkey, rpc_response));
                                    if acc_pubkey != clock::ID {
                                        inc_program_subscription_account_updates_count(
                                            &client_id,
                                        );
                                    }
                                    let _ = subscription_updates_sender.send(sub_update)
                                        .await
                                        .inspect_err(|err| {
                                            error!(error = ?err, pubkey = %acc_pubkey, "Failed to send program subscription update");
                                        });
                                }
                            }
                        } else {
                            if cancellation_token.is_cancelled() {
                                break;
                            }
                            static SIGNAL_PROGRAM_CONNECTION_COUNT: AtomicU16 =
                                AtomicU16::new(0);
                            log_trace_debug(
                                "Program subscription ended; attempting connection-local recovery",
                                "Program subscriptions ended; attempted connection-local recovery",
                                &client_id,
                                &RemoteAccountProviderError::ConnectionDisrupted,
                                100,
                                &SIGNAL_PROGRAM_CONNECTION_COUNT,
                            );
                            if let Some(unsub) = unsubscribe.take() {
                                if tokio::time::timeout(Duration::from_secs(2), unsub())
                                    .await
                                    .is_err()
                                {
                                    warn!(timeout_ms = 2000, "Unsubscribe timed out while recovering program subscription");
                                }
                            }
                            match Self::subscribe_program_with_retry(
                                pubsub_connection.as_ref(),
                                program_pubkey,
                                config.clone(),
                                retries,
                            )
                            .await
                            {
                                Ok((new_stream, new_unsub)) => {
                                    update_stream = new_stream;
                                    unsubscribe = Some(new_unsub);
                                    continue;
                                }
                                Err(err) => {
                                    warn!(
                                        client_id = %client_id,
                                        program_id = %program_pubkey,
                                        retries_count = retries,
                                        error = ?err,
                                        "Failed to recover dropped program subscription",
                                    );
                                    if Self::is_connection_level_error(&err) {
                                        Self::abort_and_signal_connection_issue(
                                            &client_id,
                                            subs.clone(),
                                            program_subs.clone(),
                                            abort_sender.clone(),
                                            is_connected.clone(),
                                            &format!(
                                                "Failed to recover dropped program subscription for {program_pubkey} after {retries} retries"
                                            ),
                                        );
                                        return;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            // Clean up subscription with timeout to prevent hanging on dead sockets
            if let Some(unsub) = unsubscribe.take() {
                if tokio::time::timeout(Duration::from_secs(2), unsub())
                    .await
                    .is_err()
                {
                    warn!(
                        timeout_ms = 2000,
                        "Unsubscribe timed out for program"
                    );
                }
            }
            program_subs
                .lock()
                .expect("program_subs lock poisoned")
                .remove(&program_pubkey);
        });
    }

    #[instrument(skip_all, fields(client_id = %client_id))]
    async fn try_reconnect(
        pubsub_connection: Arc<PubSubConnectionPool<PubsubConnectionImpl>>,
        pubsub_client_config: PubsubClientConfig,
        subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        program_subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        client_id: &str,
        is_connected: Arc<AtomicBool>,
    ) -> RemoteAccountProviderResult<()> {
        // 1. Ensure we cleaned all existing subscriptions
        Self::unsubscribe_all(subs, program_subs);

        // 2. Try to reconnect the pubsub connection
        pubsub_connection.reconnect().await?;
        // Make a sub to any account and unsub immediately to verify connection
        let pubkey = Pubkey::new_unique();
        let config = RpcAccountInfoConfig {
            commitment: Some(pubsub_client_config.commitment_config),
            encoding: Some(UiAccountEncoding::Base64Zstd),
            ..Default::default()
        };

        // 3. Try to subscribe to an account to verify connection
        let (_, unsubscribe) =
            match pubsub_connection.account_subscribe(&pubkey, config).await {
                Ok(res) => res,
                Err(err) => {
                    error!(
                        error = ?err,
                        "Failed to verify connection via subscribe"
                    );
                    return Err(err.into());
                }
            };

        // 4. Unsubscribe immediately
        unsubscribe().await;

        // 5. We are now connected again
        is_connected.store(true, Ordering::SeqCst);
        Ok(())
    }

    #[instrument(skip(subscriptions, program_subs, abort_sender, is_connected), fields(client_id = %client_id))]
    fn abort_and_signal_connection_issue(
        client_id: &str,
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        program_subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        abort_sender: mpsc::Sender<()>,
        is_connected: Arc<AtomicBool>,
        reason: &str,
    ) {
        // Only abort if we were connected; prevents duplicate aborts
        if !is_connected.swap(false, Ordering::SeqCst) {
            trace!("Already disconnected, skipping abort");
            return;
        }

        debug!(
            client_id = client_id,
            reason = reason,
            "Aborting connection"
        );

        fn drain_subscriptions(
            _client_id: &str,
            subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        ) {
            let drained_subs = {
                let mut subs_lock =
                    subscriptions.lock().expect("subscriptions lock poisoned");
                std::mem::take(&mut *subs_lock)
            };
            let drained_len = drained_subs.len();
            for (_, AccountSubscription { cancellation_token }) in drained_subs
            {
                cancellation_token.cancel();
            }
            debug!(count = drained_len, "Canceled subscriptions");
        }

        drain_subscriptions(client_id, subscriptions);
        drain_subscriptions(client_id, program_subs);

        // Use try_send to avoid blocking and naturally coalesce signals
        let _ = abort_sender.try_send(()).inspect_err(|err| {
            // Channel full is expected when reconnect is already in progress
            if !matches!(err, mpsc::error::TrySendError::Full(_)) {
                error!(
                    error = ?err,
                    "Failed to signal connection issue",
                )
            }
        });
    }
}
