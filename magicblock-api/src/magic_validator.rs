use std::{
    collections::HashMap,
    num::NonZeroU64,
    path::Path,
    sync::{Arc, atomic::AtomicU64},
    thread,
    time::{Duration, Instant},
};

use engine::Engine;
use keeper::builder::{
    AccountsDBParams, Authority, BlockstoreParams, KeeperBuilder, LedgerParams,
};
use magicblock_aperture::{
    initialize_aperture,
    state::{NodeContext, SharedState},
};
use magicblock_chainlink::{
    ProdChainlink, ProdInnerChainlink, cloner::ChainlinkCloner,
    config::ChainlinkConfig, remote_account_provider::Endpoints,
};
use magicblock_committor_service::{
    ComputeBudgetConfig, DEFAULT_ACTIONS_TIMEOUT,
    committor_processor::CommittorProcessor,
    config::ChainConfig,
    service::{IntentExecutionService, intent_client::InternalIntentRpcClient},
};
use magicblock_config::{
    ValidatorParams,
    config::{
        ChainOperationConfig, LifecycleMode, LoadableProgram,
        validator::ReplicationMode,
    },
};
use magicblock_ledger_deprecated::Ledger;
use magicblock_metrics::MetricsService;
use magicblock_program::{
    init_magic_sys,
    magicblock_processor::{
        CallbackEntrypoint, CrankEntrypoint, Entrypoint,
        PostDelegationActionEntrypoint,
    },
};
use magicblock_services::{
    actions_callback_service::ActionsCallbackService,
    undelegation_request_service::UndelegationRequestService,
};
use magicblock_task_scheduler::{SchedulerDatabase, TaskSchedulerService};
use magicblock_validator_admin::claim_fees::{ClaimFeesTask, claim_fees};
use mdp::state::{
    features::FeaturesSet,
    record::{CountryCode, ErRecord},
    status::ErStatus,
    version::v0::RecordV0,
};
use nucleus::shutdown::ShutdownManager;
use replicator::{ReplicationClient, ReplicationDispatcher};
use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_keypair::Keypair;
use solana_native_token::LAMPORTS_PER_SOL;
use solana_program_runtime::{
    invoke_context::BuiltinFunctionWithContext,
    solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk_ids::system_program;
use solana_signer::Signer;
use solana_sysvar::rent::Rent;
use tokio::{runtime::Builder, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::*;

use crate::{
    domain_registry_manager::DomainRegistryManager,
    errors::{ApiError, ApiResult},
    fund_account::initial_accounts,
    ledger::{
        self, read_validator_keypair_from_ledger, validator_keypair_path,
        write_validator_keypair_to_ledger,
    },
    magic_sys_adapter::MagicSysAdapter,
};

type InnerChainlinkImpl = ProdInnerChainlink<ChainlinkCloner>;

type ChainlinkImpl = ProdChainlink<ChainlinkCloner>;

type IntentExecutionServiceImpl =
    IntentExecutionService<InternalIntentRpcClient>;

const REPLICATION_PACER_CAPACITY: usize = 16;

// -----------------
// MagicValidator
// -----------------
pub struct MagicValidator {
    config: ValidatorParams,
    token: CancellationToken,
    engine: Engine,
    shutdown: ShutdownManager,
    /// Deprecated history remains read-only and is consulted only after engine misses.
    ledger: Arc<Ledger>,
    intent_execution_service: Option<IntentExecutionServiceImpl>,
    undelegation_request_service: Option<Arc<UndelegationRequestService>>,
    rpc_handle: thread::JoinHandle<()>,
    identity: Pubkey,
    _metrics: MetricsService,
    claim_fees_task: ClaimFeesTask,
    task_scheduler: Option<TaskSchedulerService>,
    unregister_handle: Option<thread::JoinHandle<()>>,
}

impl MagicValidator {
    // -----------------
    // Initialization
    // -----------------
    #[instrument(skip_all, fields(last_slot = tracing::field::Empty))]
    pub async fn try_from_config(config: ValidatorParams) -> ApiResult<Self> {
        let token = CancellationToken::new();
        let identity_keypair = config.validator.keypair.insecure_clone();
        let validator_pubkey = identity_keypair.pubkey();

        let engine_accounts = config.storage.join("accountsdb");
        let engine_ledger = config.storage.join("ledger");
        if config.accountsdb.reset {
            remove_directory(&engine_accounts, "engine accountsdb")?;
        }
        if config.ledger.reset {
            remove_directory(&engine_ledger, "engine ledger")?;
        }
        if config.accountsdb.defragment_on_startup {
            warn!(
                "accountsdb defragmentation is not supported by the engine storage backend"
            );
        }

        let step_start = Instant::now();
        let (ledger, _) = ledger::init(&config.storage, &config.ledger)?;
        let ledger = Arc::new(ledger);
        log_timing("startup", "init_deprecated_ledger", step_start);
        let ledger_path = ledger.ledger_path();

        let step_start = Instant::now();
        Self::sync_validator_keypair_with_ledger(
            ledger_path,
            &identity_keypair,
            config.ledger.verify_keypair,
        )?;
        log_timing("startup", "sync_validator_keypair", step_start);

        let superblock = NonZeroU64::new(config.ledger.superblock_size)
            .ok_or(ApiError::InvalidSuperblockSize)?;
        let mut shutdown = ShutdownManager::default();
        let remote_authority = match &config.validator.replication_mode {
            ReplicationMode::Replica {
                upstream_authority, ..
            } => Some(upstream_authority.0),
            ReplicationMode::Standalone | ReplicationMode::Primary { .. } => {
                None
            }
        };
        let represented_authority =
            remote_authority.unwrap_or(validator_pubkey);
        let is_replica = matches!(
            config.validator.replication_mode,
            ReplicationMode::Replica { .. }
        );
        let (pacer_tx, pacer_rx) = if is_replica {
            let (tx, rx) = mpsc::channel(REPLICATION_PACER_CAPACITY);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let builder = KeeperBuilder {
            authority: Authority {
                local: Arc::new(identity_keypair),
                remote: remote_authority,
            },
            accountsdb: AccountsDBParams {
                directory: engine_accounts,
            },
            ledger: LedgerParams {
                directory: engine_ledger,
                size_limit: config.ledger.size,
            },
            blockstore: BlockstoreParams {
                blocktime: config.ledger.block_time,
                superblock,
            },
            builtins: builtins(),
            programs: programs_to_load(&config.programs)?,
            accounts: initial_accounts(represented_authority),
            rent: Rent::default(),
        };
        let step_start = Instant::now();
        let engine = Engine::new(builder, pacer_rx, &mut shutdown).await?;
        log_timing("startup", "engine_init", step_start);

        let replication = match &config.validator.replication_mode {
            ReplicationMode::Standalone => Ok(()),
            ReplicationMode::Primary {
                bind_address,
                allowed_followers,
            } => {
                let allowed = allowed_followers
                    .iter()
                    .map(|follower| follower.0)
                    .collect::<Vec<_>>()
                    .into();
                ReplicationDispatcher::spawn(
                    bind_address.0,
                    engine.clone(),
                    allowed,
                    &mut shutdown,
                )
                .await
            }
            ReplicationMode::Replica {
                upstream_address, ..
            } => ReplicationClient::spawn(
                *upstream_address,
                engine.clone(),
                pacer_tx.expect("replica engine must have an external pacer"),
                &mut shutdown,
            ),
        };
        if let Err(error) = replication {
            shutdown.terminate().await;
            return Err(error.into());
        }

        let shared_chain_slot =
            (!Self::replication_mode_uses_disabled_chainlink(
                &config.validator.replication_mode,
            ))
            .then(Arc::<AtomicU64>::default);

        let step_start = Instant::now();
        let chainlink = Arc::new(
            Self::init_chainlink(&config, &engine, shared_chain_slot.clone())
                .await?,
        );
        log_timing("startup", "chainlink_init", step_start);

        let step_start = Instant::now();
        let committor_processor = {
            let processor = Self::init_committor_processor(
                &config,
                &engine,
                &shared_chain_slot,
            )?;
            Arc::new(processor)
        };
        let intent_execution_service = (!is_replica).then(|| {
            Self::init_intent_execution_service(
                &chainlink,
                &engine,
                &committor_processor,
                config.ledger.block_time,
                &token,
            )
        });
        log_timing("startup", "committor_service_init", step_start);
        init_magic_sys(Arc::new(MagicSysAdapter::new(
            tokio::runtime::Handle::current(),
            committor_processor.clone(),
        )));

        let step_start = Instant::now();
        let metrics_service = magicblock_metrics::try_start_metrics_service(
            config.metrics.address.0,
            token.clone(),
        )
        .map_err(ApiError::FailedToStartMetricsService)?;
        log_timing("startup", "metrics_service_start", step_start);

        let undelegation_request_service = (!matches!(
            config.validator.replication_mode,
            ReplicationMode::Replica { .. }
        ))
        .then(|| {
            Arc::new(UndelegationRequestService::new(
                chainlink.clone(),
                engine.clone(),
                config.chainlink.undelegation_request_poll_interval,
            ))
        });
        let base_fee = config.validator.basefee;
        // Faucet keypair is only used for airdrops, which are not allowed in
        // the Ephemeral mode by setting the faucet to None in node context
        // (used by the RPC implementation), we effectively disable airdrops
        let node_context = NodeContext {
            identity: represented_authority,
            is_primary: !is_replica,
            base_fee,
            featureset: Arc::new(engine.features().clone()),
            blocktime: config.ledger.block_time_ms(),
        };

        let shared_state = SharedState::new(
            node_context,
            engine.clone(),
            ledger.clone(),
            chainlink.clone(),
        );
        let step_start = Instant::now();
        let rpc =
            initialize_aperture(&config.aperture, shared_state, token.clone())
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

        let task_scheduler_db_path = SchedulerDatabase::path(&config.storage);
        debug!(path = %task_scheduler_db_path.display(), "Initializing task scheduler");
        let step_start = Instant::now();
        let task_scheduler = (!is_replica)
            .then(|| {
                TaskSchedulerService::new(
                    &task_scheduler_db_path,
                    &config.task_scheduler,
                    engine.clone(),
                    config.ledger.block_time,
                    token.clone(),
                )
            })
            .transpose()?;
        log_timing("startup", "task_scheduler_init", step_start);

        Ok(Self {
            config,
            _metrics: metrics_service,
            engine,
            shutdown,
            intent_execution_service,
            undelegation_request_service,
            token,
            ledger,
            claim_fees_task: ClaimFeesTask::new(),
            rpc_handle,
            identity: represented_authority,
            task_scheduler,
            unregister_handle: None,
        })
    }

    pub fn init_committor_processor(
        config: &ValidatorParams,
        engine: &Engine,
        shared_chain_slot: &Option<Arc<AtomicU64>>,
    ) -> ApiResult<CommittorProcessor> {
        let authority = config.validator.keypair.insecure_clone();
        let committor_persist_path =
            config.storage.join("committor_service.sqlite");
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
        config: &ValidatorParams,
        engine: &Engine,
        chain_slot: Option<Arc<AtomicU64>>,
    ) -> ApiResult<ChainlinkImpl> {
        if Self::replication_mode_uses_disabled_chainlink(
            &config.validator.replication_mode,
        ) {
            return ChainlinkImpl::disabled().map_err(ApiError::from);
        }

        let endpoints = Endpoints::try_from(config.remotes.as_slice())
            .map_err(|e| {
                ApiError::from(
                    magicblock_chainlink::errors::ChainlinkError::from(e),
                )
            })?;

        let cloner = ChainlinkCloner::new(engine.clone());
        let cloner = Arc::new(cloner);
        let accounts_bank = Arc::new(engine.clone());
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

    fn replication_mode_uses_disabled_chainlink(
        replication_mode: &ReplicationMode,
    ) -> bool {
        matches!(replication_mode, ReplicationMode::Replica { .. })
    }

    fn replication_mode_manages_onchain_registration(
        replication_mode: &ReplicationMode,
    ) -> bool {
        matches!(
            replication_mode,
            ReplicationMode::Standalone | ReplicationMode::Primary { .. }
        )
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
    #[instrument(skip(engine, config))]
    async fn register_validator_on_chain(
        engine: &Engine,
        rpc_url: &str,
        config: &ChainOperationConfig,
        block_time_ms: u64,
        base_fee: u64,
    ) -> ApiResult<()> {
        let country_code = CountryCode::from(config.country_code.alpha3());
        let validator_keypair = engine.signer().insecure_clone();
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
        {
            return;
        }

        let rpc_url = self.config.rpc_url().to_owned();
        let step_start = Instant::now();
        let validator_keypair = self.engine.signer().insecure_clone();
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
        let engine = self.engine.clone();
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
                &engine,
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
            if let Some(ref config) = chain_operation_config
                && !config.claim_fees_frequency.is_zero()
            {
                let step_start = Instant::now();
                if let Err(err) = claim_fees(&engine, rpc_url.clone()).await {
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
            if let Some(ref config) = chain_operation_config {
                let step_start = Instant::now();
                if let Err(error) = MagicValidator::register_validator_on_chain(
                    &engine,
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
        if matches!(self.config.lifecycle, LifecycleMode::Ephemeral)
            && Self::replication_mode_manages_onchain_registration(
                &self.config.validator.replication_mode,
            )
        {
            self.spawn_primary_onchain_setup();
        }

        if let Some(service) = self.undelegation_request_service.as_ref() {
            service.start();
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
            self.claim_fees_task.start(
                self.engine.clone(),
                frequency,
                self.config.rpc_url().to_owned(),
            );
            log_timing("startup", "claim_fees_task_start", step_start);
        }

        if let Some(service) = self.intent_execution_service.as_mut() {
            let step_start = Instant::now();
            service.start()?;
            log_timing("startup", "intent_execution_service_start", step_start);
        }

        // TODO: we should shutdown gracefully.
        // This is discussed in this comment:
        // https://github.com/magicblock-labs/magicblock-validator/pull/493#discussion_r2324560798
        // However there is no proper solution for this right now.
        // An issue to create a shutdown system is open here:
        // https://github.com/magicblock-labs/magicblock-validator/issues/524
        if let Some(task_scheduler) = self.task_scheduler.take() {
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
        if let Some(service) = self.intent_execution_service.as_mut()
            && let Err(err) = service.stop().await
        {
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
        self.shutdown.terminate().await;
        log_timing("shutdown", "engine_shutdown", step_start);

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

    /// Stops request ingress before the caller begins the blocking shutdown path.
    pub fn prepare_ledger_for_shutdown(&mut self) {
        let step_start = Instant::now();
        self.token.cancel();
        log_timing("shutdown", "prepare_ledger_for_shutdown", step_start);
    }
}

fn log_timing(phase: &'static str, step: &'static str, start: Instant) {
    let duration_ms = start.elapsed().as_millis() as u64;
    info!(phase, step, duration_ms, "Validator timing");
}

fn remove_directory(path: &Path, name: &'static str) -> ApiResult<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {
            info!(name, path = %path.display(), "reset storage directory");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn programs_to_load(
    programs: &[LoadableProgram],
) -> ApiResult<HashMap<Pubkey, Vec<u8>>> {
    programs
        .iter()
        .map(|program| {
            std::fs::read(&program.path)
                .map(|elf| (program.id.0, elf))
                .map_err(|err| {
                    ApiError::FailedToLoadProgramsIntoBank(format!(
                        "failed to read {} at {}: {err}",
                        program.id.0,
                        program.path.display()
                    ))
                })
        })
        .collect()
}

fn builtins() -> HashMap<Pubkey, BuiltinFunctionWithContext> {
    use magicblock_program::magicblock_processor::EphemeralSystemEntrypoint;

    let mut builtins = HashMap::<Pubkey, BuiltinFunctionWithContext>::new();
    builtins.insert(
        system_program::ID,
        (
            solana_system_program::system_processor::Entrypoint::vm,
            solana_system_program::system_processor::Entrypoint::codegen,
        ),
    );
    builtins.insert(
        magicblock_program::ID,
        (Entrypoint::vm, Entrypoint::codegen),
    );
    builtins.insert(
        magicblock_program::CRANK_PROGRAM_ID,
        (CrankEntrypoint::vm, CrankEntrypoint::codegen),
    );
    builtins.insert(
        magicblock_program::CALLBACK_PROGRAM_ID,
        (CallbackEntrypoint::vm, CallbackEntrypoint::codegen),
    );
    builtins.insert(
        magicblock_program::POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID,
        (
            PostDelegationActionEntrypoint::vm,
            PostDelegationActionEntrypoint::codegen,
        ),
    );
    builtins.insert(
        magicblock_program::EPHEMERAL_SYSTEM_PROGRAM_ID,
        (
            EphemeralSystemEntrypoint::vm,
            EphemeralSystemEntrypoint::codegen,
        ),
    );
    builtins
}

#[cfg(test)]
mod tests {
    use magicblock_config::types::{BindAddress, SerdePubkey};

    use super::*;

    fn primary_mode() -> ReplicationMode {
        ReplicationMode::Primary {
            bind_address: BindAddress("127.0.0.1:10000".parse().unwrap()),
            allowed_followers: vec![SerdePubkey(Pubkey::new_unique())],
        }
    }

    #[test]
    fn standalone_replication_mode_uses_enabled_chainlink() {
        assert!(!MagicValidator::replication_mode_uses_disabled_chainlink(
            &ReplicationMode::Standalone,
        ));
    }

    #[test]
    fn primary_replication_mode_uses_enabled_chainlink() {
        assert!(!MagicValidator::replication_mode_uses_disabled_chainlink(
            &primary_mode(),
        ));
    }

    #[test]
    fn replica_replication_mode_uses_disabled_chainlink() {
        assert!(MagicValidator::replication_mode_uses_disabled_chainlink(
            &ReplicationMode::Replica {
                upstream_address: "127.0.0.1:10000".parse().unwrap(),
                upstream_authority: SerdePubkey(Pubkey::new_unique()),
            },
        ));
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
                &primary_mode(),
            )
        );
    }

    #[test]
    fn replica_replication_mode_does_not_manage_onchain_registration() {
        assert!(
            !MagicValidator::replication_mode_manages_onchain_registration(
                &ReplicationMode::Replica {
                    upstream_address: "127.0.0.1:10000".parse().unwrap(),
                    upstream_authority: SerdePubkey(Pubkey::new_unique()),
                },
            )
        );
    }
}
