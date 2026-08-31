use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use engine::Engine;
use magicblock_aperture::{SharedState, initialize_aperture};
use magicblock_chainlink::{
    ProdChainlink, config::ChainlinkConfig, errors::ChainlinkError,
    remote_account_provider::Endpoints,
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
use magicblock_task_scheduler::{SchedulerDatabase, TaskSchedulerService};
use magicblock_validator_admin::claim_fees::{claim_fees, run_claim_fees_loop};
use nucleus::{
    metrics::EventTimer,
    shutdown::{Service, ShutdownManager, ShutdownReason},
};
use replicator::ReplicationDispatcher;
use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_native_token::LAMPORTS_PER_SOL;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
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
    engine: Engine,
    shutdown: ShutdownManager,
    intent_execution_service: Option<IntentExecutionServiceImpl>,
    undelegation_request_service: Option<UndelegationRequestService>,
    metrics: Option<MetricsService>,
    task_scheduler: Option<TaskSchedulerService>,
}

impl Leader {
    // -----------------
    // Initialization
    // -----------------
    #[instrument(skip_all, fields(last_slot = tracing::field::Empty))]
    pub async fn try_from_config(config: LeaderParams) -> ApiResult<Self> {
        let mut timer = EventTimer::new("init");

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
            let _ = shutdown.terminate().await;
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
        );
        timer.record("Committor service initialized");
        init_magic_sys(Arc::new(MagicSysAdapter::new(
            tokio::runtime::Handle::current(),
            committor_processor.clone(),
        )));
        timer.record("Magic syscall adapter initialized");

        let metrics = MetricsService::bind(config.metrics.address.0)
            .await
            .map_err(ApiError::FailedToStartMetricsService)?;
        timer.record("Metrics service bound");

        let undelegation_request_service = UndelegationRequestService::new(
            chainlink.clone(),
            engine.clone(),
            config.chainlink.undelegation_request_poll_interval,
        );
        let shared_state = SharedState::new(
            engine.clone(),
            ledger.clone(),
            chainlink.clone(),
            config.engine.blockstore.blocktime.as_millis() as u64,
        );
        let mut rpc_shutdown = shutdown.handle(Service::Rpc);
        let rpc = initialize_aperture(
            &config.aperture,
            shared_state,
            rpc_shutdown.child(),
        )
        .await?;
        timer.record("RPC service initialized");
        tokio::spawn(async move {
            rpc.run().await;
            let reason = if rpc_shutdown.requested() {
                ShutdownReason::Signalled
            } else {
                ShutdownReason::Unexpected
            };
            rpc_shutdown.terminate(reason);
        });

        let task_scheduler_db_path = SchedulerDatabase::path(&engine_ledger);
        debug!(path = %task_scheduler_db_path.display(), "Initializing task scheduler");
        let task_scheduler = Some(TaskSchedulerService::new(
            &task_scheduler_db_path,
            &config.task_scheduler,
            engine.clone(),
            config.engine.blockstore.blocktime,
        )?);
        timer.record("Task scheduler initialized");

        Ok(Self {
            config,
            engine,
            shutdown,
            intent_execution_service: Some(intent_execution_service),
            undelegation_request_service: Some(undelegation_request_service),
            metrics: Some(metrics),
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
    ) -> IntentExecutionServiceImpl {
        let intent_client = InternalIntentRpcClient::new(engine.clone());

        IntentExecutionServiceImpl::new(
            chainlink.clone(),
            intent_client,
            committor_processor.clone(),
            slot_interval,
        )
    }

    #[instrument(skip_all)]
    async fn init_chainlink(
        config: &LeaderParams,
        engine: &Engine,
        chain_slot: Arc<AtomicU64>,
    ) -> ApiResult<ChainlinkImpl> {
        let endpoints = Endpoints::try_from(config.remotes.as_slice())
            .map_err(ChainlinkError::from)?;

        let mut chainlink_config = ChainlinkConfig::default_with_lifecycle_mode(
            LifecycleMode::Ephemeral,
        );
        chainlink_config.remote_account_provider = chainlink_config
            .remote_account_provider
            .with_resubscription_delay(config.chainlink.resubscription_delay)
            .map(|conf| conf.with_grpc(config.grpc.clone()))
            .map_err(ChainlinkError::from)?;
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
                Box::new(err),
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
                    Box::new(err),
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
                        Box::new(err),
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
                        tx_err,
                    ),
                    None => ApiError::FailedToInitMagicFeeVault(
                        validator_pubkey,
                        Box::new(err),
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
                        Box::new(err),
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
                        tx_err,
                    ),
                    None => ApiError::FailedToDelegateMagicFeeVault(
                        validator_pubkey,
                        Box::new(err),
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

    fn spawn_primary_onchain_setup(&mut self) {
        let engine = self.engine.clone();
        let rpc_url = self.config.rpc_url().to_owned();
        let identity = self.engine.authority();
        let admin = self.config.admin.clone();

        let mut shutdown = self.shutdown.handle(Service::OnchainSetup);
        // Ephemeral mode does a non-blocking startup balance check.
        tokio::spawn(async move {
            let setup = async move {
                let mut timer = EventTimer::new("onchain-setup");
                Self::with_onchain_setup_retries(
                    "ensure_funded_on_chain",
                    || {
                        Leader::ensure_validator_funded_on_chain(
                            rpc_url.clone(),
                            identity,
                        )
                    },
                )
                .await?;
                timer.record("Validator balance checked");

                Self::with_onchain_setup_retries(
                    "ensure_magic_fee_vault_on_chain",
                    || {
                        Leader::ensure_magic_fee_vault_on_chain(
                            &engine,
                            rpc_url.clone(),
                        )
                    },
                )
                .await?;
                timer.record("Magic fee vault setup attempt completed");

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
                ApiResult::Ok(())
            };
            let result = tokio::select! {
                _ = shutdown.signalled() => {
                    debug!("On-chain setup cancelled by shutdown");
                    ApiResult::Ok(())
                }
                result = setup => result,
            };
            let reason = match result {
                Err(error) => ShutdownReason::Error(Box::new(error)),
                Ok(()) => {
                    shutdown.signalled().await;
                    ShutdownReason::Signalled
                }
            };
            shutdown.terminate(reason);
        });
    }

    #[instrument(skip(self))]
    pub fn start(&mut self) {
        let mut timer = EventTimer::new("startup");
        if matches!(self.config.lifecycle, LifecycleMode::Ephemeral) {
            self.spawn_primary_onchain_setup();
        }

        let undelegation_request_service = self
            .undelegation_request_service
            .take()
            .expect("undelegation request service starts once");
        let shutdown = self.shutdown.handle(Service::UndelegationRequests);
        tokio::spawn(undelegation_request_service.run(shutdown));
        timer.record("Undelegation request service started");

        // Now we are ready to start all services and are ready to accept transactions
        if let Some(frequency) = self
            .config
            .admin
            .as_ref()
            .filter(|co| !co.claim_fees_frequency.is_zero())
            .map(|co| co.claim_fees_frequency)
        {
            let engine = self.engine.clone();
            let rpc_url = self.config.rpc_url().to_owned();
            let shutdown = self.shutdown.handle(Service::FeeClaim);
            tokio::spawn(run_claim_fees_loop(
                engine, shutdown, frequency, rpc_url,
            ));
            timer.record("Fee claim task started");
        }

        let intent_execution_service = self
            .intent_execution_service
            .take()
            .expect("intent execution service starts once");
        let shutdown = self.shutdown.handle(Service::IntentExecution);
        tokio::spawn(intent_execution_service.run(shutdown));
        timer.record("Intent execution service started");

        if let Some(task_scheduler) = self.task_scheduler.take() {
            let shutdown = self.shutdown.handle(Service::TaskScheduler);
            tokio::spawn(task_scheduler.run(shutdown));
            timer.record("Task scheduler started");
        }

        let metrics = self.metrics.take().expect("metrics service starts once");
        let shutdown = self.shutdown.handle(Service::Metrics);
        tokio::spawn(metrics.run(shutdown));
        timer.record("Metrics service started");
    }

    #[instrument(skip(self))]
    pub async fn wait(&mut self) -> ShutdownReason {
        let reason = self.shutdown.wait().await;
        reason.combine(self.shutdown.terminate().await)
    }
}
