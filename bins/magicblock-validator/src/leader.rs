use std::{
    sync::{Arc, atomic::AtomicU64},
    thread,
    time::Duration,
};

use engine::Engine;
use magicblock_aperture::{SharedState, initialize_aperture};
use magicblock_chainlink::{
    ProdChainlink, config::ChainlinkConfig, remote_account_provider::Endpoints,
};
use magicblock_committor_service::{
    ComputeBudgetConfig, DEFAULT_ACTIONS_TIMEOUT,
    committor_processor::CommittorProcessor,
    config::ChainConfig,
    service::{IntentExecutionService, intent_client::InternalIntentRpcClient},
};
use magicblock_config::{LeaderParams, config::LifecycleMode};
use magicblock_metrics::MetricsService;
use magicblock_program::{init_magic_sys, validator::init_validator_authority};
use magicblock_runtime::keeper_builder;
use magicblock_services::{
    actions_callback_service::ActionsCallbackService,
    undelegation_request_service::UndelegationRequestService,
};
use magicblock_task_scheduler::TaskSchedulerService;
use magicblock_validator_admin::claim_fees::{ClaimFeesTask, claim_fees};
use nucleus::{metrics::EventTimer, shutdown::ShutdownManager};
use replicator::ReplicationDispatcher;
use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_native_token::LAMPORTS_PER_SOL;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use tokio::runtime::Builder;
use tokio_util::sync::CancellationToken;
use tracing::*;

use crate::{
    errors::{ApiError, ApiResult},
    ledger,
    magic_sys_adapter::MagicSysAdapter,
};

type ChainlinkImpl = ProdChainlink;

type IntentExecutionServiceImpl =
    IntentExecutionService<InternalIntentRpcClient>;

// -----------------
// Leader
// -----------------
pub struct Leader {
    config: LeaderParams,
    token: CancellationToken,
    engine: Engine,
    pub(crate) shutdown: ShutdownManager,
    intent_execution_service: IntentExecutionServiceImpl,
    undelegation_request_service: Arc<UndelegationRequestService>,
    rpc_handle: thread::JoinHandle<()>,
    _metrics: MetricsService,
    claim_fees_task: ClaimFeesTask,
    task_scheduler: Option<TaskSchedulerService>,
}

impl Leader {
    // -----------------
    // Initialization
    // -----------------
    #[instrument(skip_all, fields(last_slot = tracing::field::Empty))]
    pub async fn try_from_config(config: LeaderParams) -> ApiResult<Self> {
        let mut timer = EventTimer::new("init");
        let token = CancellationToken::new();

        let engine_ledger = config.engine.ledger.directory.clone();
        init_validator_authority(config.engine.authority.local.clone());

        let (ledger, _) = ledger::init(&engine_ledger, &config.ledger)?;
        let ledger = Arc::new(ledger);
        timer.record("Deprecated ledger initialized");

        let mut shutdown = ShutdownManager::default();
        let builder = keeper_builder(&config.engine, &config.programs)?;
        timer.record("Keeper runtime configured");
        let engine = Engine::new(builder, None, &mut shutdown).await?;
        timer.record("Engine initialized");

        let allowed = config
            .engine
            .replication
            .allowed_followers
            .iter()
            .map(|follower| follower.0)
            .collect::<Vec<_>>()
            .into();
        let replication = ReplicationDispatcher::spawn(
            config.engine.replication.bind_address.0,
            engine.clone(),
            allowed,
            &mut shutdown,
        )
        .await;
        if let Err(error) = replication {
            shutdown.terminate().await;
            return Err(error.into());
        }
        timer.record("Replication dispatcher started");

        let shared_chain_slot = Arc::<AtomicU64>::default();

        let chainlink = Arc::new(
            Self::init_chainlink(&config, &engine, shared_chain_slot.clone())
                .await?,
        );
        timer.record("Chainlink initialized");

        let committor_processor = {
            let processor = Self::init_committor_processor(
                &config,
                &engine,
                &Some(shared_chain_slot),
            )?;
            Arc::new(processor)
        };
        let intent_execution_service = Self::init_intent_execution_service(
            &chainlink,
            &engine,
            &committor_processor,
            config.engine.blockstore.blocktime,
            &token,
        );
        timer.record("Committor service initialized");
        init_magic_sys(Arc::new(MagicSysAdapter::new(
            tokio::runtime::Handle::current(),
            committor_processor.clone(),
        )));
        timer.record("Magic syscall adapter initialized");

        let metrics_service = magicblock_metrics::try_start_metrics_service(
            config.metrics.address.0,
            token.clone(),
        )
        .map_err(ApiError::FailedToStartMetricsService)?;
        timer.record("Metrics service started");

        let undelegation_request_service =
            Arc::new(UndelegationRequestService::new(
                chainlink.clone(),
                engine.clone(),
                config.chainlink.undelegation_request_poll_interval,
            ));
        let shared_state = SharedState::new(
            engine.clone(),
            ledger.clone(),
            chainlink.clone(),
            config.engine.blockstore.blocktime.as_millis() as u64,
        );
        let rpc =
            initialize_aperture(&config.aperture, shared_state, token.clone())
                .await?;
        timer.record("RPC service initialized");
        let rpc_handle = thread::spawn(move || {
            let workers = (num_cpus::get() / 2).saturating_sub(1).max(1);
            let runtime = Builder::new_multi_thread()
                .worker_threads(workers)
                .enable_all()
                .thread_name("rpc-worker")
                .build()
                .expect("failed to bulid async runtime for rpc service");
            runtime.block_on(rpc.run());

            drop(runtime);
            info!("RPC runtime shutdown");
        });

        debug!("Initializing task scheduler");
        let task_scheduler = Some(TaskSchedulerService::new(
            engine.clone(),
            config.aperture.listen.http(),
            config.engine.blockstore.blocktime,
            token.clone(),
        )?);
        timer.record("Task scheduler initialized");

        Ok(Self {
            config,
            _metrics: metrics_service,
            engine,
            shutdown,
            intent_execution_service,
            undelegation_request_service,
            token,
            claim_fees_task: ClaimFeesTask::new(),
            rpc_handle,
            task_scheduler,
        })
    }

    pub fn init_committor_processor(
        config: &LeaderParams,
        engine: &Engine,
        shared_chain_slot: &Option<Arc<AtomicU64>>,
    ) -> ApiResult<CommittorProcessor> {
        let authority = config.engine.authority.local.insecure_clone();
        let committor_persist_path = config
            .engine
            .ledger
            .directory
            .join("committor_service.sqlite");
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
            config.engine.authority.local.insecure_clone(),
            engine.clone(),
        );
        Ok(CommittorProcessor::try_new(
            authority,
            committor_persist_path,
            base_chain_config,
            shared_chain_slot.clone(),
            actions_callback_executor,
        )?)
    }

    fn init_intent_execution_service(
        chainlink: &Arc<ChainlinkImpl>,
        engine: &Engine,
        committor_processor: &Arc<CommittorProcessor>,
        slot_interval: Duration,
        cancellation_token: &CancellationToken,
    ) -> IntentExecutionServiceImpl {
        let intent_client = InternalIntentRpcClient::new(engine.clone());

        IntentExecutionServiceImpl::new(
            chainlink.clone(),
            intent_client,
            committor_processor.clone(),
            slot_interval,
            cancellation_token.clone(),
        )
    }

    #[instrument(skip_all)]
    async fn init_chainlink(
        config: &LeaderParams,
        engine: &Engine,
        chain_slot: Arc<AtomicU64>,
    ) -> ApiResult<ChainlinkImpl> {
        let endpoints = Endpoints::try_from(config.remotes.as_slice())
            .map_err(|e| {
                ApiError::from(
                    magicblock_chainlink::errors::ChainlinkError::from(e),
                )
            })?;

        let mut chainlink_config = ChainlinkConfig::default_with_lifecycle_mode(
            LifecycleMode::Ephemeral,
        );
        chainlink_config.remote_account_provider = chainlink_config
            .remote_account_provider
            .with_resubscription_delay(config.chainlink.resubscription_delay)
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
        ChainlinkImpl::try_new_from_endpoints(
            &endpoints,
            commitment_config,
            engine.clone(),
            config.engine.authority.local.insecure_clone(),
            chainlink_config,
            &config.chainlink,
            chain_slot,
        )
        .await
        .map_err(ApiError::from)
    }

    // -----------------
    // Start/Stop
    // -----------------
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

    async fn ensure_magic_fee_vault_on_chain(
        engine: &Engine,
        rpc_url: String,
    ) -> ApiResult<()> {
        let validator_keypair = engine.signer().insecure_clone();
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
            rpc.send_and_confirm_transaction(&tx).await.map_err(
                |err| match err.get_transaction_error() {
                    Some(tx_err) => ApiError::OnchainSetupTransactionRejected(
                        validator_pubkey,
                        tx_err.to_string(),
                    ),
                    None => ApiError::FailedToInitMagicFeeVault(
                        validator_pubkey,
                        err.to_string(),
                    ),
                },
            )?;
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
            rpc.send_and_confirm_transaction(&tx).await.map_err(
                |err| match err.get_transaction_error() {
                    Some(tx_err) => ApiError::OnchainSetupTransactionRejected(
                        validator_pubkey,
                        tx_err.to_string(),
                    ),
                    None => ApiError::FailedToDelegateMagicFeeVault(
                        validator_pubkey,
                        err.to_string(),
                    ),
                },
            )?;
            info!(%validator_pubkey, "Magic fee vault delegated");
        } else {
            info!(%validator_pubkey, "Magic fee vault already delegated, skipping");
        }

        Ok(())
    }

    /// Retries a transient on-chain setup failure with backoff; definitive
    /// outcomes like insufficient funds surface immediately.
    async fn with_onchain_setup_retries<F, Fut>(
        step: &str,
        op: F,
    ) -> ApiResult<()>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = ApiResult<()>>,
    {
        const MAX_ATTEMPTS: u32 = 5;
        let mut delay = Duration::from_secs(2);
        let mut attempt = 0;
        loop {
            attempt += 1;
            match op().await {
                Ok(()) => return Ok(()),
                Err(
                    err @ (ApiError::ValidatorInsufficientlyFunded(..)
                    | ApiError::OnchainSetupTransactionRejected(..)),
                ) => return Err(err),
                Err(err) if attempt < MAX_ATTEMPTS => {
                    warn!(
                        step,
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        retry_in_secs = delay.as_secs(),
                        error = ?err,
                        "On-chain setup step failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn spawn_primary_onchain_setup(&self) {
        let engine = self.engine.clone();
        let rpc_url = self.config.rpc_url().to_owned();
        let identity = self.engine.authority();
        let admin = self.config.admin.clone();
        let token = self.token.clone();

        // Ephemeral mode does a non-blocking startup balance check.
        // Intentionally fire-and-forget: the task itself exits the process on failure.
        tokio::spawn(async move {
            let setup = async move {
                let mut timer = EventTimer::new("onchain-setup");
                let result = Self::with_onchain_setup_retries(
                    "ensure_funded_on_chain",
                    || {
                        Leader::ensure_validator_funded_on_chain(
                            rpc_url.clone(),
                            identity,
                        )
                    },
                )
                .await;
                timer.record("Validator balance checked");
                if let Err(err) = result {
                    error!(error = ?err, "Validator balance check failed");
                    error!("Exiting process");
                    std::process::exit(1);
                }

                let result = Self::with_onchain_setup_retries(
                    "ensure_magic_fee_vault_on_chain",
                    || {
                        Leader::ensure_magic_fee_vault_on_chain(
                            &engine,
                            rpc_url.clone(),
                        )
                    },
                )
                .await;
                timer.record("Magic fee vault setup attempt completed");

                // Without magic fee vault being properly set up
                // transactions scheduling commits will fail
                if let Err(err) = result {
                    error!(error = ?err, "Magic fee vault setup failed");
                    error!("Exiting process");
                    std::process::exit(1);
                }

                if let Some(ref config) = admin
                    && !config.claim_fees_frequency.is_zero()
                {
                    if let Err(err) = claim_fees(&engine, rpc_url.clone()).await
                    {
                        error!(
                            error = ?err,
                            "Failed to claim validator fees on startup"
                        );
                    }
                    timer.record("Startup fee claim attempt completed");
                }
            };
            tokio::select! {
                _ = token.cancelled() => {
                    debug!("On-chain setup cancelled by shutdown")
                }
                _ = setup => {}
            }
        });
    }

    #[instrument(skip(self))]
    pub async fn start(&mut self) -> ApiResult<()> {
        let mut timer = EventTimer::new("startup");
        if matches!(self.config.lifecycle, LifecycleMode::Ephemeral) {
            self.spawn_primary_onchain_setup();
        }

        self.undelegation_request_service.start();
        timer.record("Undelegation request service started");

        // Now we are ready to start all services and are ready to accept transactions
        if let Some(frequency) = self
            .config
            .admin
            .as_ref()
            .filter(|co| !co.claim_fees_frequency.is_zero())
            .map(|co| co.claim_fees_frequency)
        {
            self.claim_fees_task.start(
                self.engine.clone(),
                frequency,
                self.config.rpc_url().to_owned(),
            );
            timer.record("Fee claim task started");
        }

        self.intent_execution_service.start()?;
        timer.record("Intent execution service started");

        // TODO: we should shutdown gracefully.
        // This is discussed in this comment:
        // https://github.com/magicblock-labs/magicblock-validator/pull/493#discussion_r2324560798
        // However there is no proper solution for this right now.
        // An issue to create a shutdown system is open here:
        // https://github.com/magicblock-labs/magicblock-validator/issues/524
        if let Some(task_scheduler) = self.task_scheduler.take() {
            tokio::spawn(async move {
                let join_handle = {
                    let mut timer = EventTimer::new("task-scheduler");
                    let handle = match task_scheduler.start().await {
                        Ok(join_handle) => join_handle,
                        Err(err) => {
                            error!(error = ?err, "Failed to start task scheduler");
                            error!("Exiting process");
                            std::process::exit(1);
                        }
                    };
                    timer.record("Task scheduler started");
                    handle
                };

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
        let mut timer = EventTimer::new("shutdown");

        // Stop request ingress before stopping intent execution so shutdown
        // does not admit new local undelegation scheduling work.
        self.token.cancel();

        let _ = self.rpc_handle.join();
        timer.record("RPC thread stopped");

        self.undelegation_request_service.stop();
        timer.record("Undelegation request service stopped");

        if let Err(err) = self.intent_execution_service.stop().await {
            error!(error =? err, "Failure during stopping Intent Execution Service")
        }
        timer.record("Intent execution service stopped");

        self.claim_fees_task.stop().await;
        timer.record("Fee claim task stopped");

        self.shutdown.terminate().await;
        timer.record("Validator shutdown completed");
    }
}
