use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use magicblock_account_cloner::{
    map_committor_request_result, ChainlinkCloner,
};
use magicblock_accounts::{
    scheduled_commits_processor::ScheduledCommitsProcessorImpl,
    ScheduledCommitsProcessor,
};
use magicblock_accounts_db::AccountsDb;
use magicblock_aperture::{
    initialize_aperture,
    state::{NodeContext, SharedState},
};
use magicblock_chainlink::{
    config::ChainlinkConfig,
    remote_account_provider::{
        chain_rpc_client::ChainRpcClientImpl,
        chain_updates_client::ChainUpdatesClient, Endpoints,
    },
    submux::SubMuxClient,
    Chainlink,
};
use magicblock_committor_service::{
    config::ChainConfig, BaseIntentCommittor, CommittorService,
    ComputeBudgetConfig,
};
use magicblock_config::{
    config::{
        ChainOperationConfig, LedgerConfig, LifecycleMode, LoadableProgram,
    },
    ValidatorParams,
};
use magicblock_core::{
    link::{
        blocks::BlockUpdateTx, link, transactions::TransactionSchedulerHandle,
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
    validator::{self, validator_authority},
    TransactionScheduler as ActionTransactionScheduler,
};
use magicblock_task_scheduler::{SchedulerDatabase, TaskSchedulerService};
use magicblock_validator_admin::claim_fees::ClaimFeesTask;
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
use tokio::runtime::Builder;
use tokio_util::sync::CancellationToken;
use tracing::*;

use crate::{
    domain_registry_manager::DomainRegistryManager,
    errors::{ApiError, ApiResult},
    fund_account::{
        fund_ephemeral_vault, fund_magic_context, funded_faucet,
        init_validator_identity,
    },
    genesis_utils::{create_genesis_config_with_leader, GenesisConfigInfo},
    ledger::{
        self, read_validator_keypair_from_ledger, validator_keypair_path,
        write_validator_keypair_to_ledger,
    },
    slot::advance_slot_and_update_ledger,
    tickers::{init_slot_ticker, init_system_metrics_ticker},
};

type ChainlinkImpl = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
    AccountsDb,
    ChainlinkCloner,
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
    slot_ticker: Option<tokio::task::JoinHandle<()>>,
    committor_service: Option<Arc<CommittorService>>,
    scheduled_commits_processor: Option<Arc<ScheduledCommitsProcessorImpl>>,
    chainlink: Arc<ChainlinkImpl>,
    rpc_handle: thread::JoinHandle<()>,
    identity: Pubkey,
    transaction_scheduler: TransactionSchedulerHandle,
    block_udpate_tx: BlockUpdateTx,
    _metrics: (MetricsService, tokio::task::JoinHandle<()>),
    claim_fees_task: ClaimFeesTask,
    task_scheduler: Option<TaskSchedulerService>,
    transaction_execution: thread::JoinHandle<()>,
}

impl MagicValidator {
    // -----------------
    // Initialization
    // -----------------
    #[instrument(skip_all, fields(last_slot = tracing::field::Empty))]
    pub async fn try_from_config(config: ValidatorParams) -> ApiResult<Self> {
        // TODO(thlorenz): this will need to be recreated on each start
        let token = CancellationToken::new();
        let identity_keypair = config.validator.keypair.insecure_clone();

        let validator_pubkey = identity_keypair.pubkey();
        let GenesisConfigInfo {
            genesis_config,
            validator_pubkey,
        } = create_genesis_config_with_leader(
            u64::MAX,
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
        let step_start = Instant::now();
        let accountsdb =
            AccountsDb::new(&config.accountsdb, &config.storage, last_slot)?;
        log_timing("startup", "accountsdb_init", step_start);
        for (pubkey, account) in genesis_config.accounts {
            let _ = accountsdb.insert_account(&pubkey, &account.into());
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

        let faucet_keypair =
            funded_faucet(&accountsdb, ledger.ledger_path().as_path())?;

        let accountsdb = Arc::new(accountsdb);

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

        let (mut dispatch, validator_channels) = link();

        let step_start = Instant::now();
        let committor_service = Self::init_committor_service(&config).await?;
        log_timing("startup", "committor_service_init", step_start);
        let step_start = Instant::now();
        let chainlink = Arc::new(
            Self::init_chainlink(
                &config,
                committor_service.clone(),
                &dispatch.transaction_scheduler,
                &ledger.latest_block().clone(),
                &accountsdb,
                faucet_keypair.pubkey(),
            )
            .await?,
        );
        log_timing("startup", "chainlink_init", step_start);

        let scheduled_commits_processor =
            committor_service.as_ref().map(|committor_service| {
                Arc::new(ScheduledCommitsProcessorImpl::new(
                    committor_service.clone(),
                    chainlink.clone(),
                    dispatch.transaction_scheduler.clone(),
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
        let base_fee = config.validator.basefee;
        let txn_scheduler_state = TransactionSchedulerState {
            accountsdb: accountsdb.clone(),
            ledger: ledger.clone(),
            transaction_status_tx: validator_channels.transaction_status,
            txn_to_process_rx: validator_channels.transaction_to_process,
            account_update_tx: validator_channels.account_update,
            environment: build_svm_env(&accountsdb, latest_block.blockhash, 0),
            tasks_tx: validator_channels.tasks_service,
            is_auto_airdrop_lamports_enabled: config
                .chainlink
                .auto_airdrop_lamports
                > 0,
            shutdown: token.clone(),
        };
        TRANSACTION_COUNT.inc_by(ledger.count_transactions()? as u64);
        // Faucet keypair is only used for airdrops, which are not allowed in
        // the Ephemeral mode by setting the faucet to None in node context
        // (used by the RPC implementation), we effectively disable airdrops
        let faucet = (config.lifecycle != LifecycleMode::Ephemeral)
            .then_some(faucet_keypair);
        let node_context = NodeContext {
            identity: validator_pubkey,
            faucet,
            base_fee,
            featureset: txn_scheduler_state.environment.feature_set.clone(),
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
            let workers = (num_cpus::get() / 2 - 1).max(1);
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
            // NOTE: set during [Self::start]
            slot_ticker: None,
            committor_service,
            scheduled_commits_processor,
            chainlink,
            token,
            ledger,
            ledger_truncator,
            claim_fees_task: ClaimFeesTask::new(),
            rpc_handle,
            identity: validator_pubkey,
            transaction_scheduler: dispatch.transaction_scheduler,
            block_udpate_tx: validator_channels.block_update,
            task_scheduler: Some(task_scheduler),
            transaction_execution,
        })
    }

    #[instrument(skip(config))]
    async fn init_committor_service(
        config: &ValidatorParams,
    ) -> ApiResult<Option<Arc<CommittorService>>> {
        let committor_persist_path =
            config.storage.join("committor_service.sqlite");
        debug!(path = %committor_persist_path.display(), "Initializing committor service");
        // TODO(thlorenz): when we support lifecycle modes again, only start it when needed
        let committor_service = Some(Arc::new(CommittorService::try_start(
            config.validator.keypair.insecure_clone(),
            committor_persist_path,
            ChainConfig {
                rpc_uri: config.rpc_url().to_owned(),
                commitment: CommitmentConfig::confirmed(),
                compute_budget_config: ComputeBudgetConfig::new(
                    config.commit.compute_unit_price,
                ),
            },
        )?));

        if let Some(committor_service) = &committor_service {
            if config.chainlink.prepare_lookup_tables {
                debug!("Reserving common pubkeys");
                map_committor_request_result(
                    committor_service.reserve_common_pubkeys(),
                    committor_service.clone(),
                )
                .await?;
            }
        }
        Ok(committor_service)
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(
        config,
        committor_service,
        transaction_scheduler,
        latest_block,
        accountsdb
    ))]
    async fn init_chainlink(
        config: &ValidatorParams,
        committor_service: Option<Arc<CommittorService>>,
        transaction_scheduler: &TransactionSchedulerHandle,
        latest_block: &LatestBlock,
        accountsdb: &Arc<AccountsDb>,
        faucet_pubkey: Pubkey,
    ) -> ApiResult<ChainlinkImpl> {
        let endpoints = Endpoints::try_from(config.remotes.as_slice())
            .map_err(|e| {
                ApiError::from(
                    magicblock_chainlink::errors::ChainlinkError::from(e),
                )
            })?;

        let cloner = ChainlinkCloner::new(
            committor_service,
            config.chainlink.clone(),
            transaction_scheduler.clone(),
            accountsdb.clone(),
            latest_block.clone(),
        );
        let cloner = Arc::new(cloner);
        let accounts_bank = accountsdb.clone();
        let mut chainlink_config =
            ChainlinkConfig::default_with_lifecycle_mode(
                config.lifecycle.clone(),
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
        let chainlink = ChainlinkImpl::try_new_from_endpoints(
            &endpoints,
            commitment_config,
            &accounts_bank,
            &cloner,
            config.validator.keypair.pubkey(),
            faucet_pubkey,
            chainlink_config,
            &config.chainlink,
        )
        .await?;

        Ok(chainlink)
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
        // In case if we have a perfect match between accountsdb and ledger slot
        // (note: that accountsdb is always 1 slot ahead of ledger), then there's
        // no need to run any kind of ledger replay
        if accountsdb_slot.saturating_sub(1) == ledger_slot {
            return Ok(());
        }
        // SOLANA only allows blockhash to be valid for 150 slot back in time,
        // considering that the average slot time on solana is 400ms, then:
        const SOLANA_VALID_BLOCKHASH_AGE: u64 = 150 * 400;
        // we have this number for our max blockhash age in slots, which correspond to 60 seconds
        let max_block_age =
            SOLANA_VALID_BLOCKHASH_AGE / self.config.ledger.block_time_ms();
        let step_start = Instant::now();
        let slot_to_continue_at = process_ledger(
            &self.ledger,
            accountsdb_slot,
            self.transaction_scheduler.clone(),
            max_block_age,
        )
        .await?;
        log_timing("startup", "ledger_replay", step_start);
        self.accountsdb.set_slot(slot_to_continue_at);

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

        // We want the next transaction either due to hydrating of cloned accounts or
        // user request to be processed in the next slot such that it doesn't become
        // part of the last block found in the existing ledger which would be incorrect.
        let step_start = Instant::now();
        let (update_ledger_result, _) = advance_slot_and_update_ledger(
            &self.accountsdb,
            &self.ledger,
            &self.block_udpate_tx,
        );
        if let Err(err) = update_ledger_result {
            return Err(err.into());
        }
        log_timing("startup", "advance_slot_after_replay", step_start);

        tracing::Span::current().record("ledger_slot", slot_to_continue_at);
        info!("Ledger processing complete");

        Ok(())
    }

    #[instrument(skip(self, config), fields(identity = %self.identity))]
    async fn register_validator_on_chain(
        &self,
        config: &ChainOperationConfig,
    ) -> ApiResult<()> {
        let country_code = CountryCode::from(config.country_code.alpha3());
        let validator_keypair = validator_authority();
        let validator_info = ErRecord::V0(RecordV0 {
            identity: validator_keypair.pubkey(),
            status: ErStatus::Active,
            block_time_ms: self.config.ledger.block_time_ms() as u16,
            base_fee: self.config.validator.basefee as u16,
            features: FeaturesSet::default(),
            load_average: 0, // not implemented
            country_code,
            addr: config.fqdn.to_string(),
        });

        DomainRegistryManager::handle_registration_static(
            self.config.rpc_url(),
            &validator_keypair,
            validator_info,
        )
        .map_err(|err| {
            ApiError::FailedToRegisterValidatorOnChain(format!("{:?}", err))
        })
    }

    fn unregister_validator_on_chain(&self) -> ApiResult<()> {
        let validator_keypair = validator_authority();

        DomainRegistryManager::handle_unregistration_static(
            self.config.rpc_url(),
            &validator_keypair,
        )
        .map_err(|err| {
            ApiError::FailedToUnregisterValidatorOnChain(format!("{err:#}"))
        })
        .inspect(|_| info!("Unregistered validator on chain"))
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

    #[instrument(skip(self))]
    pub async fn start(&mut self) -> ApiResult<()> {
        if matches!(self.config.lifecycle, LifecycleMode::Ephemeral) {
            let rpc_url = self.config.rpc_url().to_owned();
            let identity = self.identity;
            // Ephemeral mode does a non-blocking startup balance check.
            // Intentionally fire-and-forget: the task itself exits the process on failure,
            tokio::spawn(async move {
                let step_start = Instant::now();
                let result = MagicValidator::ensure_validator_funded_on_chain(
                    rpc_url, identity,
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
            });
            if let Some(ref config) = self.config.chain_operation {
                let step_start = Instant::now();
                self.register_validator_on_chain(config).await?;
                log_timing(
                    "startup",
                    "register_validator_on_chain",
                    step_start,
                );
            }
        }

        // Ledger processing needs to happen before anything of the below
        let step_start = Instant::now();
        self.maybe_process_ledger().await?;
        log_timing("startup", "maybe_process_ledger", step_start);

        // Ledger replay has completed, we can now clean non-delegated accounts
        // including programs from the bank
        if !self.config.accountsdb.reset {
            let step_start = Instant::now();
            self.chainlink.reset_accounts_bank()?;
            log_timing("startup", "reset_accounts_bank", step_start);
        }

        // Now we are ready to start all services and are ready to accept transactions
        if let Some(frequency) = self
            .config
            .chain_operation
            .as_ref()
            .filter(|co| !co.claim_fees_frequency.is_zero())
            .map(|co| co.claim_fees_frequency)
        {
            let step_start = Instant::now();
            self.claim_fees_task
                .start(frequency, self.config.rpc_url().to_owned());
            log_timing("startup", "claim_fees_task_start", step_start);
        }

        let step_start = Instant::now();
        self.slot_ticker = Some(init_slot_ticker(
            self.accountsdb.clone(),
            &self.scheduled_commits_processor,
            self.ledger.clone(),
            self.config.ledger.block_time,
            self.transaction_scheduler.clone(),
            self.block_udpate_tx.clone(),
            self.exit.clone(),
        ));
        log_timing("startup", "slot_ticker_start", step_start);

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

        validator::finished_starting_up();
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn stop(mut self) {
        let stop_start = Instant::now();
        self.exit.store(true, Ordering::Relaxed);

        // Ordering is important here
        // Commitor service shall be stopped last
        self.token.cancel();
        if let Some(ref scheduled_commits_processor) =
            self.scheduled_commits_processor
        {
            let step_start = Instant::now();
            scheduled_commits_processor.stop();
            log_timing(
                "shutdown",
                "scheduled_commits_processor_stop",
                step_start,
            );
        }
        if let Some(ref committor_service) = self.committor_service {
            let step_start = Instant::now();
            committor_service.stop();
            log_timing("shutdown", "committor_service_stop", step_start);
        }

        let step_start = Instant::now();
        self.claim_fees_task.stop().await;
        log_timing("shutdown", "claim_fees_task_stop", step_start);

        if self.config.chain_operation.is_some()
            && matches!(self.config.lifecycle, LifecycleMode::Ephemeral)
        {
            let step_start = Instant::now();
            if let Err(err) = self.unregister_validator_on_chain() {
                error!(error = ?err, "Failed to unregister");
            }
            log_timing("shutdown", "unregister_validator_on_chain", step_start);
        }
        // we have two memory mapped databases,
        // flush them to disk before exitting
        let step_start = Instant::now();
        self.accountsdb.flush();
        log_timing("shutdown", "accountsdb_flush", step_start);

        let step_start = Instant::now();
        if let Err(err) = self.ledger.shutdown(true) {
            error!(error = ?err, "Failed to shutdown ledger");
        }
        log_timing("shutdown", "ledger_shutdown", step_start);
        let step_start = Instant::now();
        let _ = self.rpc_handle.join();
        log_timing("shutdown", "rpc_thread_join", step_start);
        if let Some(handle) = self.slot_ticker {
            let step_start = Instant::now();
            let _ = handle.await;
            log_timing("shutdown", "slot_ticker_join", step_start);
        }
        let step_start = Instant::now();
        if let Err(err) = self.ledger_truncator.join() {
            error!(error = ?err, "Ledger truncator did not gracefully exit");
        }
        log_timing("shutdown", "ledger_truncator_join", step_start);
        let step_start = Instant::now();
        let _ = self.transaction_execution.join();
        log_timing("shutdown", "transaction_execution_join", step_start);

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
    debug!(phase, step, duration_ms, "Validator timing");
}

fn programs_to_load(programs: &[LoadableProgram]) -> Vec<(Pubkey, PathBuf)> {
    programs
        .iter()
        .map(|program| (program.id.0, program.path.clone()))
        .collect()
}
