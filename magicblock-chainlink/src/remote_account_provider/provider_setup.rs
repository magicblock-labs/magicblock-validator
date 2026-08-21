use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status_client_types::UiConfirmedBlock;

use super::*;

impl
    RemoteAccountProvider<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>>
{
    pub async fn try_from_urls_and_config(
        endpoints: &Endpoints,
        commitment: CommitmentConfig,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        chain_slot: Option<Arc<AtomicU64>>,
    ) -> ChainlinkResult<
        Option<
            RemoteAccountProvider<
                ChainRpcClientImpl,
                SubMuxClient<ChainUpdatesClient>,
            >,
        >,
    > {
        let mode = config.lifecycle_mode();
        if mode.needs_remote_account_provider() {
            debug!("Creating RemoteAccountProvider");
            let provider = RemoteAccountProvider::<
                ChainRpcClientImpl,
                SubMuxClient<ChainUpdatesClient>,
            >::try_new_from_endpoints(
                endpoints,
                commitment,
                subscription_forwarder,
                config,
                chain_slot.unwrap_or_default(),
            )
            .await?;
            Ok(Some(provider))
        } else {
            Ok(None)
        }
    }
}

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    pub(super) fn next_fetching_account_generation(
        &self,
    ) -> FetchingAccountGeneration {
        self.next_fetching_account_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    pub async fn try_from_clients_and_mode(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        subscribed_accounts: Arc<SubscribedAccounts>,
        chain_slot: Arc<AtomicU64>,
    ) -> ChainlinkResult<Option<RemoteAccountProvider<T, U>>> {
        let chain_slot = ChainSlot::new(chain_slot);
        if config.lifecycle_mode().needs_remote_account_provider() {
            Ok(Some(
                Self::new(
                    rpc_client,
                    pubsub_client,
                    subscription_forwarder,
                    config,
                    subscribed_accounts,
                    chain_slot,
                )
                .await?,
            ))
        } else {
            Ok(None)
        }
    }

    /// Creates a background task that periodically reconciles subscriptions
    /// with the pubsub tracking (repairing missing ones, e.g. after a partial
    /// resubscription) and optionally updates the active subscriptions gauge
    pub(super) fn start_active_subscriptions_updater<
        PubsubClient: ChainPubsubClient,
    >(
        subscribed_accounts: Arc<SubscribedAccounts>,
        pubsub_client: Arc<PubsubClient>,
        stale_account_tx: mpsc::Sender<Pubkey>,
        subscription_key_locks: SubscriptionKeyLocks,
        subscription_ownership: SubscriptionOwnershipMap,
        emit_metrics: bool,
    ) -> task::JoinHandle<()> {
        task::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(
                ACTIVE_SUBSCRIPTIONS_UPDATE_INTERVAL_MS,
            ));
            let internally_managed = subscribed_accounts.internally_managed();

            loop {
                interval.tick().await;
                let pubsub_total =
                    subscription_reconciler::reconcile_subscriptions(
                        &subscribed_accounts,
                        pubsub_client.as_ref(),
                        &internally_managed,
                        &stale_account_tx,
                        Some(&subscription_key_locks),
                        Some(&subscription_ownership),
                    )
                    .await;

                debug!(count = pubsub_total, "Updating active subscriptions");
                if emit_metrics {
                    set_monitored_accounts_count(pubsub_total);
                }
            }
        })
    }

    /// Creates a new instance of the remote account provider
    /// By the time this method returns the current chain slot was resolved and
    /// a subscription setup to keep it up to date.
    pub(crate) async fn new(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        subscribed_accounts: Arc<SubscribedAccounts>,
        chain_slot: ChainSlot,
    ) -> RemoteAccountProviderResult<Self> {
        let (stale_account_tx, stale_account_rx) =
            tokio::sync::mpsc::channel(100);
        let subscription_key_locks: SubscriptionKeyLocks =
            Arc::new(AsyncMutex::new(HashMap::new()));
        let subscription_ownership: SubscriptionOwnershipMap =
            Arc::new(AsyncMutex::new(HashMap::new()));

        // The reconciler always runs: partial resubscriptions rely on it for
        // repair. The config flag only gates the metrics emission.
        let active_subscriptions_updater =
            Some(Self::start_active_subscriptions_updater(
                subscribed_accounts.clone(),
                Arc::new(pubsub_client.clone()),
                stale_account_tx.clone(),
                subscription_key_locks.clone(),
                subscription_ownership.clone(),
                config.enable_subscription_metrics(),
            ));

        let me = Self {
            fetching_accounts: Arc::<FetchingAccounts>::default(),
            next_fetching_account_generation: AtomicU64::default(),
            subscription_ownership,
            subscription_key_locks,
            rpc_client,
            pubsub_client,
            chain_slot,
            last_update_slot: Arc::<AtomicU64>::default(),
            received_updates_count: Arc::<AtomicU64>::default(),
            subscribed_accounts,
            stale_account_tx,
            stale_account_rx: Mutex::new(Some(stale_account_rx)),
            subscription_forwarder: Arc::new(subscription_forwarder),
            replay_outbox: Arc::default(),
            replay_notify: Arc::new(Notify::new()),
            _active_subscriptions_task_handle: active_subscriptions_updater,
        };

        let updates = me.pubsub_client.take_updates();
        me.listen_for_account_updates(updates)?;
        me.start_replay_outbox_worker();
        let clock_remote_account = me
            .try_get(
                clock::ID,
                AccountFetchContext::internal(AccountFetchReason::Clock),
            )
            .await?;
        match clock_remote_account {
            RemoteAccount::NotFound(_) => {
                Err(RemoteAccountProviderError::ClockAccountCouldNotBeResolved(
                    clock::ID.to_string(),
                ))
            }
            RemoteAccount::Found(_) => {
                me.chain_slot.update(clock_remote_account.slot());
                Ok(me)
            }
        }
    }

    pub async fn try_new_from_endpoints(
        endpoints: &Endpoints,
        commitment: CommitmentConfig,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        chain_slot: Arc<AtomicU64>,
    ) -> RemoteAccountProviderResult<
        RemoteAccountProvider<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >,
    > {
        if endpoints.is_empty() {
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    "No endpoints provided".to_string(),
                ),
            );
        }

        // Build RPC clients (use the first RPC endpoint found)
        let rpc_url = endpoints.rpc_url().ok_or_else(|| {
            RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                "No RPC endpoint found".to_string(),
            )
        })?;
        let rpc_client =
            ChainRpcClientImpl::new_from_url(rpc_url.as_str(), commitment);

        let (grpc, ws): (Vec<_>, Vec<_>) = endpoints
            .pubsubs()
            .into_iter()
            .cloned()
            .partition(|endpoint| matches!(endpoint, Endpoint::Grpc { .. }));
        let grpc_selected = !grpc.is_empty();
        let selected = match config.subscription_transport() {
            SubscriptionTransport::Grpc if !grpc.is_empty() => {
                metrics::set_subscription_transport_grpc(true);
                grpc
            }
            SubscriptionTransport::Grpc => {
                error!(
                    ws_remotes = ws.len(),
                    "subscription-transport is grpc but no gRPC remote is configured; falling back to websockets (DEGRADED)"
                );
                metrics::set_subscription_transport_grpc(false);
                ws.clone()
            }
            SubscriptionTransport::Ws => {
                warn!(
                    "subscription-transport is ws: dev/test only, unsupported in production"
                );
                metrics::set_subscription_transport_grpc(false);
                ws.clone()
            }
        };

        match Self::try_new_from_pubsubs(
            selected,
            commitment,
            rpc_client.clone(),
            chain_slot.clone(),
            subscription_forwarder.clone(),
            config,
        )
        .await
        {
            Ok(provider) => Ok(provider),
            Err(err)
                if matches!(
                    config.subscription_transport(),
                    SubscriptionTransport::Grpc
                ) && grpc_selected
                    && !ws.is_empty() =>
            {
                error!(
                    error = %err,
                    "gRPC subscriptions failed at startup; running DEGRADED on websockets"
                );
                metrics::set_subscription_transport_grpc(false);
                Self::try_new_from_pubsubs(
                    ws,
                    commitment,
                    rpc_client,
                    chain_slot,
                    subscription_forwarder,
                    config,
                )
                .await
            }
            Err(err) => Err(err),
        }
    }

    pub(super) async fn try_new_from_pubsubs(
        pubsub_endpoints: Vec<Endpoint>,
        commitment: CommitmentConfig,
        rpc_client: ChainRpcClientImpl,
        chain_slot: Arc<AtomicU64>,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
    ) -> RemoteAccountProviderResult<
        RemoteAccountProvider<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >,
    > {
        let resubscription_delay = config.resubscription_delay();
        let pubsub_futs = pubsub_endpoints.into_iter().map(|ep| {
            connect_pubsub_client(
                ep,
                commitment,
                rpc_client.clone(),
                chain_slot.clone(),
                resubscription_delay,
                config.ws_subs_per_connection(),
                config.grpc().clone(),
            )
        });
        let results = join_all(pubsub_futs).await;
        let pubsubs = collect_connected_pubsubs(results);

        if pubsubs.is_empty() {
            return Err(RemoteAccountProviderError::AllPubsubClientsFailed);
        }
        let subscribed_accounts = Arc::new(SubscribedAccounts::default());

        let submux =
            SubMuxClient::new(pubsubs, subscribed_accounts.clone(), None);

        if !config.program_subs().is_empty() {
            let count = config.program_subs().len();
            debug!(count, "Subscribing to program accounts");
            let subscribe_program_futs = config
                .program_subs()
                .iter()
                .map(|program_id| submux.subscribe_program(*program_id));
            if let Err(error) = try_join_all(subscribe_program_futs).await {
                if let Err(shutdown_error) = submux.shutdown().await {
                    warn!(
                        ?shutdown_error,
                        "failed to shut down pubsub clients after program subscription failure"
                    );
                }
                return Err(error);
            }
        }

        let shutdown_submux = submux.clone();
        let provider = RemoteAccountProvider::<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >::new(
            rpc_client,
            submux,
            subscription_forwarder,
            config,
            subscribed_accounts,
            ChainSlot::new(chain_slot),
        )
        .await;
        let provider = match provider {
            Ok(provider) => provider,
            Err(error) => {
                if let Err(shutdown_error) = shutdown_submux.shutdown().await {
                    warn!(
                        ?shutdown_error,
                        "failed to shut down pubsub clients after provider startup failure"
                    );
                }
                return Err(error);
            }
        };
        Ok(provider)
    }

    pub(crate) async fn get_slot(&self) -> RemoteAccountProviderResult<u64> {
        tokio::time::timeout(RPC_FETCH_TIMEOUT, self.rpc_client.get_slot())
            .await
            .map_err(|_| {
                RemoteAccountProviderError::AccountResolutionsFailed(format!(
                    "RPC call timeout fetching slot after {}ms",
                    RPC_FETCH_TIMEOUT.as_millis()
                ))
            })?
            .map_err(|err| {
                RemoteAccountProviderError::AccountResolutionsFailed(format!(
                    "RpcError fetching slot: {err:?}"
                ))
            })
    }

    pub(crate) async fn get_program_accounts_with_config(
        &self,
        pubkey: &Pubkey,
        mut config: RpcProgramAccountsConfig,
    ) -> RemoteAccountProviderResult<Vec<(Pubkey, Account)>> {
        config.account_config.commitment = Some(self.rpc_client.commitment());

        tokio::time::timeout(RPC_FETCH_TIMEOUT, async {
            self.rpc_client
                .get_program_accounts_with_config(pubkey, config)
                .await
        })
        .await
        .map_err(|_| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RPC call timeout fetching program accounts for {pubkey} after {}ms",
                RPC_FETCH_TIMEOUT.as_millis()
            ))
        })?
        .map_err(|err| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RpcError fetching program accounts for {pubkey}: {err:?}"
            ))
        })
    }

    pub(crate) async fn get_block_with_config(
        &self,
        slot: u64,
        mut config: RpcBlockConfig,
    ) -> RemoteAccountProviderResult<UiConfirmedBlock> {
        config.commitment = Some(self.rpc_client.commitment());

        tokio::time::timeout(
            RPC_FETCH_TIMEOUT,
            self.rpc_client.get_block_with_config(slot, config),
        )
        .await
        .map_err(|_| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RPC call timeout fetching block {slot} after {}ms",
                RPC_FETCH_TIMEOUT.as_millis()
            ))
        })?
        .map_err(|err| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RpcError fetching block {slot}: {err:?}"
            ))
        })
    }

    pub fn chain_slot(&self) -> u64 {
        self.chain_slot.load()
    }

    pub fn try_get_stale_account_rx(
        &self,
    ) -> RemoteAccountProviderResult<mpsc::Receiver<Pubkey>> {
        let mut rx = self
            .stale_account_rx
            .lock()
            .expect("stale_account_rx lock poisoned");
        rx.take().ok_or_else(|| {
            RemoteAccountProviderError::StaleAccountSenderSupportsSingleReceiverOnly
        })
    }

    pub(crate) async fn send_stale_account(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        self.stale_account_tx
            .send(pubkey)
            .await
            .map_err(RemoteAccountProviderError::FailedToSendStaleAccount)
    }

    pub fn last_update_slot(&self) -> u64 {
        self.last_update_slot.load(Ordering::Relaxed)
    }

    pub fn received_updates_count(&self) -> u64 {
        self.received_updates_count.load(Ordering::Relaxed)
    }
}
