use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use magicblock_account_cloner::ChainlinkCloner;
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_aperture::{
    initialize_aperture,
    state::{NodeContext, SharedState},
};
use magicblock_chainlink::{
    config::ChainlinkConfig, remote_account_provider::Endpoints, ProdChainlink,
    ProdInnerChainlink,
};
use magicblock_committor_service::{
    committor_processor::CommittorProcessor, config::ChainConfig,
    intent_engine::db::DummyIntentBacklog,
    outbox::outbox_client::InternalOutboxClient,
    service::IntentExecutionService, ComputeBudgetConfig,
    DEFAULT_ACTIONS_TIMEOUT,
};
use magicblock_config::{
    config::{
        validator::ReplicationMode, ChainOperationConfig, LedgerConfig,
        LifecycleMode, LoadableProgram,
    },
    ValidatorParams,
};
use magicblock_core::{
    coordination_mode::CoordinationMode,
    link::{
        link,
        replication::Message,
        transactions::{SchedulerMode, TransactionSchedulerHandle},
    },
    Slot,
};
use magicblock_ledger::{
    blockstore_processor::process_ledger,
    ledger_truncator::{LedgerTruncator, DEFAULT_TRUNCATION_TIME_INTERVAL},
    LatestBlock, Ledger,
};
use magicblock_metrics::{metrics::TRANSACTION_COUNT, MetricsService};
use magicblock_processor::{
    build_svm_env,
    loader::load_upgradeable_programs,
    scheduler::{state::TransactionSchedulerState, TransactionScheduler},
};
use magicblock_program::{
    init_magic_sys,
    validator::{self, validator_authority},
    TransactionScheduler as ActionTransactionScheduler,
};
use magicblock_replicator::{nats::Broker, BrokerSource, ReplicationService};
use magicblock_services::{
    actions_callback_service::ActionsCallbackService,
    undelegation_request_service::UndelegationRequestService,
};
use magicblock_task_scheduler::{SchedulerDatabase, TaskSchedulerService};
use magicblock_validator_admin::claim_fees::{claim_fees, ClaimFeesTask};
use mdp::state::{
    features::FeaturesSet,
    record::{CountryCode, ErRecord},
    status::ErStatus,
    version::v0::RecordV0,
};
use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_keypair::Keypair;
use solana_native_token::LAMPORTS_PER_SOL;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use tokio::{
    runtime::Builder,
    sync::mpsc::{channel, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::*;

use crate::{
    domain_registry_manager::DomainRegistryManager,
    errors::{ApiError, ApiResult},
    fund_account::{
        fund_ephemeral_vault, fund_magic_context, init_validator_identity,
    },
    genesis_utils::{create_genesis_config_with_leader, GenesisConfigInfo},
    ledger::{
        self, read_validator_keypair_from_ledger, validator_keypair_path,
        write_validator_keypair_to_ledger,
    },
    magic_sys_adapter::MagicSysAdapter,
    tickers::init_system_metrics_ticker,
};

type InnerChainlinkImpl = ProdInnerChainlink<ChainlinkCloner>;

type ChainlinkImpl = ProdChainlink<ChainlinkCloner>;

type CommittorProcessorImpl = CommittorProcessor<DummyIntentBacklog>;

type IntentExecutionServiceImpl = IntentExecutionService<
    InternalOutboxClient<LatestBlock>,
    DummyIntentBacklog,
>;

// -----------------
// MagicValidator
// -----------------
pub struct MagicValidator {
    config: ValidatorParams,
    exit: Arc<AtomicBool>,
    token: CancellationToken,
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    ledger_truncator: LedgerTruncator,
    intent_execution_service: IntentExecutionServiceImpl,
    replication_service: Option<ReplicationService>,
    undelegation_request_service: Option<Arc<UndelegationRequestService>>,
    rpc_handle: thread::JoinHandle<()>,
    identity: Pubkey,
    transaction_scheduler: TransactionSchedulerHandle,
    _metrics: (MetricsService, tokio::task::JoinHandle<()>),
    claim_fees_task: ClaimFeesTask,
    task_scheduler: Option<TaskSchedulerService>,
    transaction_execution: thread::JoinHandle<()>,
    replication_handle:
        Option<thread::JoinHandle<magicblock_replicator::Result<()>>>,
    mode_tx: Sender<SchedulerMode>,
    /// `None` when replication is disabled, i.e. the validator runs standalone.
    replication_tx: Option<Sender<Message>>,
    unregister_handle: Option<thread::JoinHandle<()>>,
    is_standalone: bool,
}

impl MagicValidator {
    // -----------------
    // Initialization
    // -----------------
    #[instrument(skip_all, fields(last_slot = tracing::field::Empty))]
    pub async fn try_from_config(
        mut config: ValidatorParams,
    ) -> ApiResult<Self> {
        // TODO(thlorenz): this will need to be recreated on each start
        let token = CancellationToken::new();
        let identity_keypair = config.validator.keypair.insecure_clone();

        let validator_pubkey = identity_keypair.pubkey();
        let GenesisConfigInfo {
            accounts: genesis_accounts,
            validator_pubkey,
        } = create_genesis_config_with_leader(
            &validator_pubkey,
            config.validator.basefee,
        );

        let step_start = Instant::now();
        let (ledger, last_slot) =
            Self::init_ledger(&config.ledger, &config.storage)?;
        log_timing("startup", "init_ledger", step_start);
        tracing::Span::current().record("last_slot", last_slot);
        info!("Ledger initialized");
        let ledger_path = ledger.ledger_path();

        let step_start = Instant::now();
        Self::sync_validator_keypair_with_ledger(
            ledger_path,
            &identity_keypair,
            config.ledger.verify_keypair,
        )?;
        log_timing("startup", "sync_validator_keypair", step_start);

        let latest_block = ledger.latest_block().load();
        let mut accountsdb =
            AccountsDb::new(&config.accountsdb, &config.storage, last_slot)?;

        // Mode switch channel for transitioning from StartingUp to Primary
        // or Replica mode after ledger replay
        let (mode_tx, mode_rx) = channel(1);
        let is_standalone = matches!(
            config.validator.replication_mode,
            ReplicationMode::Standalone
        );

        // Connect to replication broker if configured.
        // Returns (broker, is_fresh_start) where is_fresh_start indicates
        // whether accountsdb was empty and may need a snapshot.
        let broker = if let Some(conf) =
            config.validator.replication_mode.config()
        {
            let is_fresh_start = accountsdb.slot() == 0;
            // A fresh node must fetch the bootstrap snapshot before ledger
            // replay, so it connects synchronously here. Otherwise the connect
            // (and producer-lock acquisition) is deferred to the replication
            // service thread
            let source = if is_fresh_start {
                let step_start = Instant::now();
                let mut broker = Broker::connect(conf.url, conf.secret).await?;
                info!(
                    "accountsdb is not initialized, trying to fetch snapshot"
                );
                if let Some(snapshot) = broker.get_snapshot().await? {
                    info!(slot = snapshot.slot, "fetched accountsdb snapshot");
                    accountsdb.insert_external_snapshot(
                        snapshot.slot,
                        &snapshot.data,
                    )?;
                    // we have essentially reset the accountsdb,
                    // and chainlink should not prune it, as it
                    // would introduce divergence with primary node
                    config.accountsdb.reset = true;
                } else {
                    warn!("no snapshot is found in replication stream");
                }
                log_timing("startup", "replication broker init", step_start);
                BrokerSource::Connected(broker)
            } else {
                BrokerSource::Pending {
                    url: conf.url,
                    secret: conf.secret,
                }
            };
            Some((source, is_fresh_start))
        } else {
            None
        };
        let accountsdb = Arc::new(accountsdb);
        let (mut dispatch, validator_channels) = link(!is_standalone);
        let shared_chain_slot =
            (!Self::is_replica_mode(&config.validator.replication_mode))
                .then(Arc::<AtomicU64>::default);

        let step_start = Instant::now();
        let chainlink = Arc::new(
            Self::init_chainlink(
                &config,
                &dispatch.transaction_scheduler,
                &ledger.latest_block().clone(),
                &accountsdb,
                shared_chain_slot.clone(),
            )
            .await?,
        );
        log_timing("startup", "chainlink_init", step_start);

        let step_start = Instant::now();
        let outbox_client = {
            let val = Self::init_outbox_client(
                &config,
                &accountsdb,
                &dispatch.transaction_scheduler,
                ledger.latest_block(),
            );
            Arc::new(val)
        };
        let committor_processor = {
            let processor = Self::init_committor_processor(
                &config,
                ledger.latest_block(),
                &accountsdb,
                &outbox_client,
                &shared_chain_slot,
            );
            Arc::new(processor)
        };
        let intent_execution_service =
            if Self::is_replica_mode(&config.validator.replication_mode) {
                // Replica mode is documented as having no side effects - the
                // accept/schedule loop must never run.
                IntentExecutionServiceImpl::disabled()
            } else {
                IntentExecutionServiceImpl::new(
                    chainlink.clone(),
                    outbox_client.clone(),
                    committor_processor.clone(),
                    config.ledger.block_time,
                    token.clone(),
                )
            };
        log_timing("startup", "committor_service_init", step_start);
        init_magic_sys(Arc::new(MagicSysAdapter::new(
            tokio::runtime::Handle::current(),
            committor_processor.clone(),
        )));

        let replication_service =
            if let Some((broker_source, is_fresh_start)) = broker {
                let messages_rx =
                    dispatch.replication_messages.take().ok_or_else(|| {
                        ApiError::FailedToSendModeSwitch(
                            "replication channel missing after init".to_owned(),
                        )
                    })?;
                Some(ReplicationService::new(
                    broker_source,
                    mode_tx.clone(),
                    accountsdb.clone(),
                    ledger.clone(),
                    dispatch.transaction_scheduler.clone(),
                    messages_rx,
                    token.clone(),
                    is_fresh_start,
                    config.validator.replication_mode.clone(),
                    validator_pubkey,
                ))
            } else {
                None
            };
        log_timing("startup", "accountsdb_init", step_start);
        for (pubkey, account) in genesis_accounts {
            if accountsdb.get_account(&pubkey).is_some() {
                continue;
            }
            let _ = accountsdb.insert_account(&pubkey, &account);
        }

        let exit = Arc::<AtomicBool>::default();
        let ledger_truncator = LedgerTruncator::new(
            ledger.clone(),
            DEFAULT_TRUNCATION_TIME_INTERVAL,
            config.ledger.size,
        );

        init_validator_identity(&accountsdb, &validator_pubkey);
        fund_magic_context(&accountsdb);
        fund_ephemeral_vault(&accountsdb);

        let step_start = Instant::now();
        let metrics_service = magicblock_metrics::try_start_metrics_service(
            config.metrics.address.0,
            token.clone(),
        )
        .map_err(ApiError::FailedToStartMetricsService)?;
        log_timing("startup", "metrics_service_start", step_start);

        let step_start = Instant::now();
        let system_metrics_ticker = init_system_metrics_ticker(
            config.metrics.collect_frequency,
            &ledger,
            &accountsdb,
            token.clone(),
        );
        log_timing("startup", "system_metrics_ticker_start", step_start);

        let undelegation_request_service = (!matches!(
            config.validator.replication_mode,
            ReplicationMode::Replica { .. }
        ))
        .then(|| {
            Arc::new(UndelegationRequestService::new(
                chainlink.clone(),
                dispatch.transaction_scheduler.clone(),
                identity_keypair.insecure_clone(),
                ledger.latest_block().clone(),
                config.chainlink.undelegation_request_poll_interval,
            ))
        });

        let step_start = Instant::now();
        load_upgradeable_programs(
            &accountsdb,
            &programs_to_load(&config.programs),
        )
        .map_err(|err| {
            ApiError::FailedToLoadProgramsIntoBank(format!("{:?}", err))
        })?;
        log_timing("startup", "load_programs", step_start);

        validator::init_validator_authority(identity_keypair);
        if let Some(pk) = config.validator.replication_mode.authority_override()
        {
            validator::set_validator_authority_override(pk);
        }
        let base_fee = config.validator.basefee;

        let svm_env = build_svm_env(&accountsdb, latest_block.blockhash, 0);
        let feature_set = svm_env.feature_set.clone();
        let txn_scheduler_state = TransactionSchedulerState {
            accountsdb: accountsdb.clone(),
            ledger: ledger.clone(),
            transaction_status_tx: validator_channels.transaction_status,
            txn_to_process_rx: validator_channels.transaction_to_process,
            account_update_tx: validator_channels.account_update,
            environment: svm_env.environment,
            feature_set: feature_set.clone(),
            tasks_tx: validator_channels.tasks_service,
            replication_tx: validator_channels.replication_messages.clone(),
            block_time: config.ledger.block_time,
            superblock_size: config.ledger.superblock_size,
            shutdown: token.clone(),
            mode_rx,
            pause_permit: validator_channels.pause_permit,
        };
        TRANSACTION_COUNT.inc_by(ledger.count_transactions()? as u64);
        // Faucet keypair is only used for airdrops, which are not allowed in
        // the Ephemeral mode by setting the faucet to None in node context
        // (used by the RPC implementation), we effectively disable airdrops
        let node_context = NodeContext {
            identity: validator_pubkey,
            base_fee,
            featureset: Arc::new(feature_set),
            blocktime: config.ledger.block_time_ms(),
        };
        // We dedicate half of the available resources to the execution
        // runtime, -1 is taken up by the transaction scheduler itself
        let transaction_executors =
            (num_cpus::get() / 2).saturating_sub(1).max(1) as u32;
        let step_start = Instant::now();
        let transaction_scheduler = TransactionScheduler::new(
            transaction_executors,
            txn_scheduler_state,
        );
        log_timing("startup", "transaction_scheduler_init", step_start);
        info!(
            executor_count = transaction_executors,
            "Running execution backend"
        );
        let step_start = Instant::now();
        let transaction_execution = transaction_scheduler.spawn();
        log_timing("startup", "transaction_execution_spawn", step_start);

        let shared_state = SharedState::new(
            node_context,
            accountsdb.clone(),
            ledger.clone(),
            chainlink.clone(),
        );
        let step_start = Instant::now();
        let rpc = initialize_aperture(
            &config.aperture,
            shared_state,
            &dispatch,
            token.clone(),
        )
        .await?;
        log_timing("startup", "aperture_init", step_start);
        let rpc_handle = thread::spawn(move || {
            let step_start = Instant::now();
            let workers = (num_cpus::get() / 2).saturating_sub(1).max(1);
            let runtime = Builder::new_multi_thread()
                .worker_threads(workers)
                .enable_all()
                .thread_name("rpc-worker")
                .build()
                .expect("failed to bulid async runtime for rpc service");
            log_timing("startup", "rpc_runtime_build", step_start);
            runtime.block_on(rpc.run());

            drop(runtime);
            info!("RPC runtime shutdown");
        });

        let task_scheduler_db_path =
            SchedulerDatabase::path(ledger.ledger_path().parent().expect(
                "ledger_path didn't have a parent, should never happen",
            ));
        debug!(path = %task_scheduler_db_path.display(), "Initializing task scheduler");
        let step_start = Instant::now();
        let task_scheduler = TaskSchedulerService::new(
            &task_scheduler_db_path,
            &config.task_scheduler,
            config.aperture.listen.http(),
            dispatch
                .tasks_service
                .take()
                .expect("tasks_service should be initialized"),
            ledger.latest_block().clone(),
            Duration::from_millis(config.ledger.block_time_ms()),
            token.clone(),
        )?;
        log_timing("startup", "task_scheduler_init", step_start);

        Ok(Self {
            accountsdb,
            config,
            exit,
            _metrics: (metrics_service, system_metrics_ticker),
            intent_execution_service,
            replication_service,
            undelegation_request_service,
            token,
            ledger,
            ledger_truncator,
            claim_fees_task: ClaimFeesTask::new(),
            rpc_handle,
            identity: validator_pubkey,
            transaction_scheduler: dispatch.transaction_scheduler,
            task_scheduler: Some(task_scheduler),
            transaction_execution,
            replication_handle: None,
            mode_tx,
            replication_tx: validator_channels.replication_messages,
            unregister_handle: None,
            is_standalone,
        })
    }

    fn init_outbox_client(
        config: &ValidatorParams,
        accounts_db: &Arc<AccountsDb>,
        transaction_scheduler: &TransactionSchedulerHandle,
        latest_block: &LatestBlock,
    ) -> InternalOutboxClient<LatestBlock> {
        let rpc_client = RpcClient::new(config.aperture.listen.http());
        InternalOutboxClient::new(
            accounts_db.clone(),
            Arc::new(rpc_client),
            transaction_scheduler.clone(),
            latest_block.clone(),
        )
    }

    pub fn init_committor_processor(
        config: &ValidatorParams,
        latest_block: &LatestBlock,
        accounts_db: &Arc<AccountsDb>,
        outbox_client: &Arc<InternalOutboxClient<LatestBlock>>,
        shared_chain_slot: &Option<Arc<AtomicU64>>,
    ) -> CommittorProcessorImpl {
        let authority = config.validator.keypair.insecure_clone();
        let base_chain_config = ChainConfig {
            rpc_uri: config.rpc_url().to_owned(),
            commitment: CommitmentConfig::confirmed(),
            websocket_uri: config
                .websocket_urls()
                .next()
                .map(ToOwned::to_owned),
            compute_budget_config: ComputeBudgetConfig::new(
                config.commit.compute_unit_price,
            ),
            actions_timeout: DEFAULT_ACTIONS_TIMEOUT,
        };

        // TODO(thlorenz): if startup roles change, revisit whether this service is needed for that role.
        let actions_callback_executor = ActionsCallbackService::new(
            Arc::new(RpcClient::new(config.aperture.listen.http())),
            config.validator.keypair.insecure_clone(),
            latest_block.clone(),
        );
        CommittorProcessor::new(
            authority,
            base_chain_config,
            shared_chain_slot.clone(),
            DummyIntentBacklog::new(accounts_db.clone()),
            outbox_client.clone(),
            actions_callback_executor,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip_all)]
    async fn init_chainlink(
        config: &ValidatorParams,
        transaction_scheduler: &TransactionSchedulerHandle,
        latest_block: &LatestBlock,
        accountsdb: &Arc<AccountsDb>,
        chain_slot: Option<Arc<AtomicU64>>,
    ) -> ApiResult<ChainlinkImpl> {
        if Self::is_replica_mode(&config.validator.replication_mode) {
            return ChainlinkImpl::disabled().map_err(ApiError::from);
        }

        let endpoints = Endpoints::try_from(config.remotes.as_slice())
            .map_err(|e| {
                ApiError::from(
                    magicblock_chainlink::errors::ChainlinkError::from(e),
                )
            })?;

        let cloner = ChainlinkCloner::new(
            transaction_scheduler.clone(),
            latest_block.clone(),
        );
        let cloner = Arc::new(cloner);
        let accounts_bank = accountsdb.clone();
        let mut chainlink_config =
            ChainlinkConfig::default_with_lifecycle_mode(
                LifecycleMode::Ephemeral,
            )
            .with_remove_confined_accounts(
                config.chainlink.remove_confined_accounts,
            );
        chainlink_config.remote_account_provider = chainlink_config
            .remote_account_provider
            .with_resubscription_delay(config.chainlink.resubscription_delay)
            .and_then(|conf| {
                conf.with_subscribed_accounts_lru_capacity(
                    config.chainlink.max_monitored_accounts,
                )
            })
            .and_then(|conf| {
                conf.with_secondary_subscriptions_lru_capacity(
                    config.chainlink.secondary_max_monitored_accounts,
                )
            })
            .map(|conf| conf.with_grpc(config.grpc.clone()))
            .map_err(|err| {
                ApiError::from(
                    magicblock_chainlink::errors::ChainlinkError::from(err),
                )
            })?;
        let commitment_config = {
            let level = CommitmentLevel::Confirmed;
            CommitmentConfig { commitment: level }
        };
        let chainlink = InnerChainlinkImpl::try_new_from_endpoints(
            &endpoints,
            commitment_config,
            &accounts_bank,
            &cloner,
            config.validator.keypair.insecure_clone(),
            chainlink_config,
            &config.chainlink,
            config.storage.as_path(),
            chain_slot.unwrap_or_default(),
        )
        .await?;

        Ok(ChainlinkImpl::enabled(chainlink))
    }

    fn is_replica_mode(replication_mode: &ReplicationMode) -> bool {
        matches!(replication_mode, ReplicationMode::Replica { .. })
    }

    fn replication_mode_manages_onchain_registration(
        replication_mode: &ReplicationMode,
    ) -> bool {
        matches!(
            replication_mode,
            ReplicationMode::Standalone | ReplicationMode::Primary(_)
        )
    }

    fn init_ledger(
        ledger_config: &LedgerConfig,
        storage: &Path,
    ) -> ApiResult<(Arc<Ledger>, Slot)> {
        let (ledger, last_slot) = ledger::init(storage, ledger_config)?;
        let ledger_shared = Arc::new(ledger);
        Ok((ledger_shared, last_slot))
    }

    fn sync_validator_keypair_with_ledger(
        ledger_path: &Path,
        validator_keypair: &Keypair,
        verify_keypair: bool,
    ) -> ApiResult<()> {
        if !validator_keypair_path(ledger_path)?.exists() {
            write_validator_keypair_to_ledger(ledger_path, validator_keypair)?;
            return Ok(());
        };
        if !verify_keypair {
            warn!("Skipping ledger keypair verification");
            return Ok(());
        }
        let existing_keypair = read_validator_keypair_from_ledger(ledger_path)?;
        if existing_keypair.ne(validator_keypair) {
            return Err(
                ApiError::LedgerValidatorKeypairNotMatchingProvidedKeypair(
                    ledger_path.display().to_string(),
                    existing_keypair.pubkey().to_string(),
                ),
            );
        }

        Ok(())
    }

    // -----------------
    // Start/Stop
    // -----------------
    #[instrument(skip(self), fields(ledger_slot = tracing::field::Empty))]
    async fn maybe_process_ledger(&self) -> ApiResult<()> {
        if self.config.ledger.reset {
            return Ok(());
        }
        let accountsdb_slot = self.accountsdb.slot();
        let ledger_slot = self.ledger.latest_block().load().slot;
        // If we have accountsdb state, which is at least as new as the last state state
        // transition in the ledger then there's no need to run any kind of ledger replay
        if accountsdb_slot >= ledger_slot {
            return Ok(());
        }

        // SOLANA only allows blockhash to be valid for 150 slot back in time,
        // considering that the average slot time on solana is 400ms, then:
        const SOLANA_VALID_BLOCKHASH_AGE: u64 = 150 * 400;
        // we have this number for our max blockhash age in slots, which correspond to 60 seconds
        let max_block_age =
            SOLANA_VALID_BLOCKHASH_AGE / self.config.ledger.block_time_ms();
        let step_start = Instant::now();
        let process_ledger_result = process_ledger(
            &self.ledger,
            accountsdb_slot,
            self.transaction_scheduler.clone(),
            max_block_age,
        )
        .await;

        let slot_to_continue_at = process_ledger_result?;
        log_timing("startup", "ledger_replay", step_start);

        // The transactions to schedule and accept account commits re-run when we
        // process the ledger, however we do not want to re-commit them.
        // Thus while the ledger is processed we don't yet run the machinery to handle
        // scheduled commits and we clear all scheduled commits before fully starting the
        // validator.
        let scheduled_commits =
            ActionTransactionScheduler::default().scheduled_actions_len();
        ActionTransactionScheduler::default().clear_scheduled_actions();
        debug!(
            committed_count = scheduled_commits,
            "Cleared scheduled commits"
        );

        tracing::Span::current().record("ledger_slot", slot_to_continue_at);
        info!("Ledger processing complete");

        Ok(())
    }

    #[instrument(skip(config))]
    async fn register_validator_on_chain(
        rpc_url: &str,
        config: &ChainOperationConfig,
        block_time_ms: u64,
        base_fee: u64,
    ) -> ApiResult<()> {
        let country_code = CountryCode::from(config.country_code.alpha3());
        let validator_keypair = validator_authority();
        let validator_info = ErRecord::V0(RecordV0 {
            identity: validator_keypair.pubkey(),
            status: ErStatus::Active,
            block_time_ms: block_time_ms as u16,
            base_fee: base_fee as u16,
            features: FeaturesSet::default(),
            load_average: 0,
            country_code,
            addr: config.fqdn.to_string(),
        });

        DomainRegistryManager::handle_registration_static(
            rpc_url,
            &validator_keypair,
            validator_info,
        )
        .await
        .map_err(|err| {
            ApiError::FailedToRegisterValidatorOnChain(err.to_string())
        })
    }

    pub async fn start_unregister_validator_on_chain(&mut self) {
        if self.unregister_handle.is_some() {
            return;
        }
        if self.config.chain_operation.is_none()
            || !Self::replication_mode_manages_onchain_registration(
                &self.config.validator.replication_mode,
            )
            || !matches!(self.config.lifecycle, LifecycleMode::Ephemeral)
            || !CoordinationMode::current().needs_onchain_interactions()
        {
            return;
        }

        let rpc_url = self.config.rpc_url().to_owned();
        let step_start = Instant::now();
        let validator_keypair = validator_authority();
        // Await send before shutdown so runtime drop can only cancel confirmation.
        let result =
            DomainRegistryManager::send_unregistration_and_confirm_in_background_static(
                rpc_url,
                &validator_keypair,
            )
            .await
            .map_err(|err| {
                ApiError::FailedToUnregisterValidatorOnChain(err.to_string())
            });
        log_timing(
            "shutdown",
            "send_unregister_validator_on_chain",
            step_start,
        );

        match result {
            Ok((signature, handle)) => {
                info!(%signature, "Sent validator unregister transaction");
                self.unregister_handle = Some(handle);
            }
            Err(err) => {
                error!(error = ?err, "Failed to send unregister");
            }
        }
    }

    async fn ensure_validator_funded_on_chain(
        rpc_url: String,
        identity: Pubkey,
    ) -> ApiResult<()> {
        // NOTE: 5 SOL seems reasonable, but we may require a different amount in the future
        const MIN_BALANCE_SOL: u64 = 5;

        let lamports = RpcClient::new_with_commitment(
            rpc_url,
            CommitmentConfig::confirmed(),
        )
        .get_balance(&identity)
        .await
        .map_err(|err| {
            ApiError::FailedToObtainValidatorOnChainBalance(
                identity,
                err.to_string(),
            )
        })?;
        if lamports < MIN_BALANCE_SOL * LAMPORTS_PER_SOL {
            Err(ApiError::ValidatorInsufficientlyFunded(
                identity,
                MIN_BALANCE_SOL,
            ))
        } else {
            Ok(())
        }
    }

    async fn ensure_magic_fee_vault_on_chain(rpc_url: String) -> ApiResult<()> {
        let validator_keypair = validator_authority();
        let validator_pubkey = validator_keypair.pubkey();
        let vault_pubkey =
            dlp_api::pda::magic_fee_vault_pda_from_validator(&validator_pubkey);
        let delegation_record_pubkey =
            dlp_api::pda::delegation_record_pda_from_delegated_account(
                &vault_pubkey,
            );

        let rpc = RpcClient::new_with_commitment(
            rpc_url,
            CommitmentConfig::confirmed(),
        );

        let accounts = rpc
            .get_multiple_accounts(&[vault_pubkey, delegation_record_pubkey])
            .await
            .map_err(|err| {
                ApiError::FailedToInitMagicFeeVault(
                    validator_pubkey,
                    err.to_string(),
                )
            })?;

        let vault_exists = accounts[0].is_some();
        let delegation_record_exists = accounts[1].is_some();

        if !vault_exists {
            info!(%validator_pubkey, "Magic fee vault absent, initializing");
            let ix = dlp_api::instruction_builder::init_magic_fee_vault(
                validator_pubkey,
                validator_pubkey,
            );
            let blockhash =
                rpc.get_latest_blockhash().await.map_err(|err| {
                    ApiError::FailedToInitMagicFeeVault(
                        validator_pubkey,
                        err.to_string(),
                    )
                })?;
            let tx = solana_transaction::Transaction::new_signed_with_payer(
                &[ix],
                Some(&validator_pubkey),
                &[&validator_keypair],
                blockhash,
            );
            rpc.send_and_confirm_transaction(&tx).await.map_err(|err| {
                ApiError::FailedToInitMagicFeeVault(
                    validator_pubkey,
                    err.to_string(),
                )
            })?;
            info!(%validator_pubkey, "Magic fee vault initialized");
        } else {
            info!(%validator_pubkey, "Magic fee vault already exists, skipping init");
        }

        if !delegation_record_exists {
            info!(%validator_pubkey, "Magic fee vault not delegated, delegating");
            let ix = dlp_api::instruction_builder::delegate_magic_fee_vault(
                validator_pubkey,
                validator_pubkey,
            );
            let blockhash =
                rpc.get_latest_blockhash().await.map_err(|err| {
                    ApiError::FailedToDelegateMagicFeeVault(
                        validator_pubkey,
                        err.to_string(),
                    )
                })?;
            let tx = solana_transaction::Transaction::new_signed_with_payer(
                &[ix],
                Some(&validator_pubkey),
                &[&validator_keypair],
                blockhash,
            );
            rpc.send_and_confirm_transaction(&tx).await.map_err(|err| {
                ApiError::FailedToDelegateMagicFeeVault(
                    validator_pubkey,
                    err.to_string(),
                )
            })?;
            info!(%validator_pubkey, "Magic fee vault delegated");
        } else {
            info!(%validator_pubkey, "Magic fee vault already delegated, skipping");
        }

        Ok(())
    }

    fn spawn_primary_onchain_setup(&self) {
        let rpc_url = self.config.rpc_url().to_owned();
        let identity = self.identity;
        let chain_operation_config = self.config.chain_operation.clone();
        let block_time_ms = self.config.ledger.block_time_ms();
        let base_fee = self.config.validator.basefee;

        // Ephemeral mode does a non-blocking startup balance check.
        // Intentionally fire-and-forget: the task itself exits the process on failure.
        tokio::spawn(async move {
            let step_start = Instant::now();
            let result = MagicValidator::ensure_validator_funded_on_chain(
                rpc_url.clone(),
                identity,
            )
            .await;
            log_timing(
                "startup_background",
                "ensure_funded_on_chain",
                step_start,
            );
            if let Err(err) = result {
                error!(error = ?err, "Validator balance check failed");
                error!("Exiting process");
                std::process::exit(1);
            }

            let step_start = Instant::now();
            let result = MagicValidator::ensure_magic_fee_vault_on_chain(
                rpc_url.clone(),
            )
            .await;
            log_timing(
                "startup_background",
                "ensure_magic_fee_vault_on_chain",
                step_start,
            );

            // Without magic fee vault being properly set up
            // transactions scheduling commits will fail
            if let Err(err) = result {
                error!(error = ?err, "Magic fee vault setup failed");
                error!("Exiting process");
                std::process::exit(1);
            }
            if let Some(ref config) = chain_operation_config {
                if !config.claim_fees_frequency.is_zero() {
                    let step_start = Instant::now();
                    if let Err(err) = claim_fees(rpc_url.clone()).await {
                        error!(
                            error = ?err,
                            "Failed to claim validator fees on startup"
                        );
                    }
                    log_timing(
                        "startup_background",
                        "claim_fees_on_startup",
                        step_start,
                    );
                }
            }
            if let Some(ref config) = chain_operation_config {
                let step_start = Instant::now();
                if let Err(error) = MagicValidator::register_validator_on_chain(
                    &rpc_url,
                    config,
                    block_time_ms,
                    base_fee,
                )
                .await
                {
                    error!(%error, "Validator registration failed, exitting");
                    std::process::exit(1);
                }
                log_timing(
                    "startup_background",
                    "register_validator_on_chain",
                    step_start,
                );
            }
        });
    }

    #[instrument(skip(self))]
    pub async fn start(&mut self) -> ApiResult<()> {
        // Ledger processing needs to happen before anything of the below
        let step_start = Instant::now();
        self.maybe_process_ledger().await?;

        log_timing("startup", "maybe_process_ledger", step_start);

        if self.config.accountsdb.defragment_on_startup {
            let step_start = Instant::now();
            // SAFETY: ledger replay has completed, and normal startup has not
            // yet enabled bank cleanup, scheduler modes, replication, slot
            // ticks, or task recovery.
            unsafe { self.accountsdb.defragment()? };
            log_timing("startup", "accountsdb_defragment", step_start);
        }

        // Ledger replay has completed; primary and standalone nodes can clean
        // stale non-delegated accounts before accepting new work. Replicas wait
        // for the primary's Reset message so they stay stream-ordered.
        if !matches!(
            self.config.validator.replication_mode,
            ReplicationMode::Replica { .. }
        ) {
            let step_start = Instant::now();
            self.accountsdb.reset_bank(&self.identity)?;
            log_timing("startup", "reset_accounts_bank", step_start);

            if matches!(
                self.config.validator.replication_mode,
                ReplicationMode::Primary(_)
            ) {
                // Take the sender: this is its only use, and the shutdown
                // drain only completes once every sender clone is dropped.
                let replication_tx =
                    self.replication_tx.take().ok_or_else(|| {
                        ApiError::FailedToSendModeSwitch(
                            "replication sink missing in primary mode"
                                .to_owned(),
                        )
                    })?;
                replication_tx
                    .send(Message::Reset(self.accountsdb.slot()))
                    .await
                    .map_err(|e| {
                        ApiError::FailedToSendModeSwitch(format!(
                            "Failed to send reset message to replication: {e}"
                        ))
                    })?;
            }
        }

        // Notify the scheduler that ledger replay and bank cleanup is complete.
        if self.is_standalone {
            self.mode_tx
                .send(SchedulerMode::Primary)
                .await
                .map_err(|e| {
                    ApiError::FailedToSendModeSwitch(format!(
                        "Failed to send primary mode to scheduler: {e}"
                    ))
                })?;
            if matches!(self.config.lifecycle, LifecycleMode::Ephemeral) {
                self.spawn_primary_onchain_setup();
            }
        } else if let Some(replicator) = self.replication_service.take() {
            self.replication_handle.replace(replicator.spawn());
            if Self::replication_mode_manages_onchain_registration(
                &self.config.validator.replication_mode,
            ) && matches!(self.config.lifecycle, LifecycleMode::Ephemeral)
            {
                self.spawn_primary_onchain_setup();
            }
        }

        if !matches!(
            self.config.validator.replication_mode,
            ReplicationMode::Replica { .. }
        ) {
            if let Some(service) = self.undelegation_request_service.as_ref() {
                service.start();
            }
        }

        // Now we are ready to start all services and are ready to accept transactions
        if let Some(frequency) = self
            .config
            .chain_operation
            .as_ref()
            .filter(|_| {
                Self::replication_mode_manages_onchain_registration(
                    &self.config.validator.replication_mode,
                )
            })
            .filter(|co| !co.claim_fees_frequency.is_zero())
            .map(|co| co.claim_fees_frequency)
        {
            let step_start = Instant::now();
            self.claim_fees_task
                .start(frequency, self.config.rpc_url().to_owned());
            log_timing("startup", "claim_fees_task_start", step_start);
        }

        let step_start = Instant::now();
        self.intent_execution_service.start()?;
        log_timing("startup", "intent_execution_service_start", step_start);

        let step_start = Instant::now();
        self.ledger_truncator.start();
        log_timing("startup", "ledger_truncator_start", step_start);

        // TODO: we should shutdown gracefully.
        // This is discussed in this comment:
        // https://github.com/magicblock-labs/magicblock-validator/pull/493#discussion_r2324560798
        // However there is no proper solution for this right now.
        // An issue to create a shutdown system is open here:
        // https://github.com/magicblock-labs/magicblock-validator/issues/524
        let task_scheduler = self
            .task_scheduler
            .take()
            .expect("task_scheduler should be initialized");
        let is_primary_mode = {
            let mut mode = CoordinationMode::current();
            while mode == CoordinationMode::StartingUp {
                tokio::select! {
                    _ = self.token.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
                mode = CoordinationMode::current();
            }
            mode == CoordinationMode::Primary
        };
        if is_primary_mode {
            tokio::spawn(async move {
                let step_start = Instant::now();
                let join_handle = match task_scheduler.start().await {
                    Ok(join_handle) => join_handle,
                    Err(err) => {
                        error!(error = ?err, "Failed to start task scheduler");
                        error!("Exiting process");
                        std::process::exit(1);
                    }
                };
                log_timing("startup", "task_scheduler_start", step_start);
                match join_handle.await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        error!(error = ?err, "Task scheduler failed");
                        error!("Exiting process");
                        std::process::exit(1);
                    }
                    Err(err) => {
                        error!(error = ?err, "Task scheduler join failed");
                        error!("Exiting process");
                        std::process::exit(1);
                    }
                }
            });
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn stop(mut self) {
        let stop_start = Instant::now();
        self.start_unregister_validator_on_chain().await;
        self.exit.store(true, Ordering::Relaxed);

        // Drop any remaining replication sender before cancelling: the
        // replication drain treats a closed channel as "producer finished"
        // and otherwise waits out its full timeout.
        self.replication_tx = None;

        // Stop request ingress before stopping intent execution so shutdown
        // does not admit new local undelegation scheduling work.
        self.token.cancel();
        if let Some(ref undelegation_request_service) =
            self.undelegation_request_service
        {
            let step_start = Instant::now();
            undelegation_request_service.stop();
            log_timing(
                "shutdown",
                "undelegation_request_service_stop",
                step_start,
            );
        }

        let step_start = Instant::now();
        if let Err(err) = self.intent_execution_service.stop().await {
            error!(error =? err, "Failure during stopping Intent Execution Service")
        }
        log_timing("shutdown", "intent_execution_service_stop", step_start);

        let step_start = Instant::now();
        self.claim_fees_task.stop().await;
        log_timing("shutdown", "claim_fees_task_stop", step_start);

        let step_start = Instant::now();
        let _ = self.rpc_handle.join();
        log_timing("shutdown", "rpc_thread_join", step_start);
        let step_start = Instant::now();
        if let Err(err) = self.ledger_truncator.join() {
            error!(error = ?err, "Ledger truncator did not gracefully exit");
        }
        log_timing("shutdown", "ledger_truncator_join", step_start);
        let step_start = Instant::now();
        let _ = self.transaction_execution.join();
        log_timing("shutdown", "transaction_execution_join", step_start);
        let step_start = Instant::now();
        if let Some(handle) = self.replication_handle {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    error!(error = ?err, "Replication service exited with error");
                }
                Err(err) => {
                    error!(panic = ?err, "Replication service thread panicked");
                }
            }
        }
        log_timing("shutdown", "replication_service_join", step_start);

        // Flush durable state only after every worker that can still admit,
        // commit, or truncate state has stopped.
        let step_start = Instant::now();
        self.accountsdb.flush();
        log_timing("shutdown", "accountsdb_flush", step_start);

        let step_start = Instant::now();
        if let Err(err) = self.ledger.flush() {
            error!(error = ?err, "Failed to flush ledger");
        }
        log_timing("shutdown", "ledger_flush", step_start);

        let step_start = Instant::now();
        if let Err(err) = self.ledger.shutdown(true) {
            error!(error = ?err, "Failed to shutdown ledger");
        }
        log_timing("shutdown", "ledger_shutdown", step_start);

        if let Some(handle) = self.unregister_handle {
            if handle.is_finished() {
                if handle.join().is_err() {
                    error!("Unregister confirmation thread panicked");
                }
            } else {
                debug!(
                    "Unregister confirmation still running; not waiting during shutdown"
                );
            }
        }

        log_timing("shutdown", "stop_total", stop_start);
        info!("MagicValidator shutdown");
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Prepares RocksDB for shutdown by cancelling all Manual compactions
    /// This speeds up `stop` as it doesn't have to await for compaction cancellation
    /// Calling this still allows to write or read from DB
    pub fn prepare_ledger_for_shutdown(&mut self) {
        let step_start = Instant::now();
        self.ledger_truncator.stop();
        // Calls & awaits until manual compaction is canceled
        self.ledger.cancel_manual_compactions();
        if let Err(err) = self.ledger.flush() {
            error!(error = ?err, "Failed to flush during shutdown preparation");
        }
        log_timing("shutdown", "prepare_ledger_for_shutdown", step_start);
    }
}

fn log_timing(phase: &'static str, step: &'static str, start: Instant) {
    let duration_ms = start.elapsed().as_millis() as u64;
    info!(phase, step, duration_ms, "Validator timing");
}

fn programs_to_load(programs: &[LoadableProgram]) -> Vec<(Pubkey, PathBuf)> {
    programs
        .iter()
        .map(|program| (program.id.0, program.path.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use magicblock_config::{
        config::validator::ReplicationConfig, types::SerdePubkey,
    };

    use super::*;

    fn replication_config() -> ReplicationConfig {
        ReplicationConfig {
            url: "nats://127.0.0.1:4222".parse().unwrap(),
            secret: "secret".to_string(),
        }
    }

    #[test]
    fn standalone_replication_mode_uses_enabled_chainlink() {
        assert!(!MagicValidator::is_replica_mode(
            &ReplicationMode::Standalone,
        ));
    }

    #[test]
    fn primary_replication_mode_uses_enabled_chainlink() {
        assert!(!MagicValidator::is_replica_mode(&ReplicationMode::Primary(
            replication_config()
        ),));
    }

    #[test]
    fn replica_is_replica_mode() {
        assert!(MagicValidator::is_replica_mode(&ReplicationMode::Replica {
            config: replication_config(),
            authority_override: SerdePubkey(Pubkey::new_unique()),
        },));
    }

    #[test]
    fn standalone_replication_mode_manages_onchain_registration() {
        assert!(
            MagicValidator::replication_mode_manages_onchain_registration(
                &ReplicationMode::Standalone,
            )
        );
    }

    #[test]
    fn primary_replication_mode_manages_onchain_registration() {
        assert!(
            MagicValidator::replication_mode_manages_onchain_registration(
                &ReplicationMode::Primary(replication_config()),
            )
        );
    }

    #[test]
    fn replica_replication_mode_does_not_manage_onchain_registration() {
        assert!(
            !MagicValidator::replication_mode_manages_onchain_registration(
                &ReplicationMode::Replica {
                    config: replication_config(),
                    authority_override: SerdePubkey(Pubkey::new_unique()),
                },
            )
        );
    }
}
