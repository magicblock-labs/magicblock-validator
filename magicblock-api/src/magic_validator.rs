use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use log::*;
use magicblock_account_cloner::{
    map_committor_request_result, ChainlinkCloner,
};
use magicblock_accounts::{
    scheduled_commits_processor::ScheduledCommitsProcessorImpl, RemoteCluster,
    ScheduledCommitsProcessor,
};
use magicblock_accounts_db::AccountsDb;
use magicblock_aperture::{
    state::{NodeContext, SharedState},
    JsonRpcServer,
};
use magicblock_chainlink::{
    config::ChainlinkConfig,
    remote_account_provider::{
        chain_rpc_client::ChainRpcClientImpl,
        chain_updates_client::ChainUpdatesClient,
    },
    submux::SubMuxClient,
    Chainlink,
};
use magicblock_committor_service::{
    config::ChainConfig, BaseIntentCommittor, CommittorService,
    ComputeBudgetConfig,
};
use magicblock_config::{
    EphemeralConfig, LedgerConfig, LedgerResumeStrategy, LifecycleMode,
    PrepareLookupTables, ProgramConfig,
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
use magicblock_metrics::MetricsService;
use magicblock_processor::{
    build_svm_env,
    scheduler::{state::TransactionSchedulerState, TransactionScheduler},
};
use magicblock_program::{
    init_persister,
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
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    domain_registry_manager::DomainRegistryManager,
    errors::{ApiError, ApiResult},
    external_config::{
        remote_cluster_from_remote, try_convert_accounts_config,
    },
    fund_account::{
        fund_magic_context, fund_task_context, funded_faucet,
        init_validator_identity,
    },
    genesis_utils::{create_genesis_config_with_leader, GenesisConfigInfo},
    ledger::{
        self, read_validator_keypair_from_ledger,
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
// MagicValidatorConfig
// -----------------
#[derive(Default)]
pub struct MagicValidatorConfig {
    pub validator_config: EphemeralConfig,
}

impl std::fmt::Debug for MagicValidatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MagicValidatorConfig")
            .field("validator_config", &self.validator_config)
            .finish()
    }
}

// -----------------
// MagicValidator
// -----------------
pub struct MagicValidator {
    config: EphemeralConfig,
    exit: Arc<AtomicBool>,
    token: CancellationToken,
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    ledger_truncator: LedgerTruncator,
    slot_ticker: Option<tokio::task::JoinHandle<()>>,
    committor_service: Option<Arc<CommittorService>>,
    scheduled_commits_processor: Option<Arc<ScheduledCommitsProcessorImpl>>,
    chainlink: Arc<ChainlinkImpl>,
    rpc_handle: JoinHandle<()>,
    identity: Pubkey,
    transaction_scheduler: TransactionSchedulerHandle,
    block_udpate_tx: BlockUpdateTx,
    _metrics: Option<(MetricsService, tokio::task::JoinHandle<()>)>,
    claim_fees_task: ClaimFeesTask,
    task_scheduler_handle: Option<tokio::task::JoinHandle<()>>,
}

impl MagicValidator {
    // -----------------
    // Initialization
    // -----------------
    pub async fn try_from_config(
        config: MagicValidatorConfig,
        identity_keypair: Keypair,
    ) -> ApiResult<Self> {
        // TODO(thlorenz): this will need to be recreated on each start
        let token = CancellationToken::new();
        let config = config.validator_config;

        let validator_pubkey = identity_keypair.pubkey();
        let GenesisConfigInfo {
            genesis_config,
            validator_pubkey,
            ..
        } = create_genesis_config_with_leader(
            u64::MAX,
            &validator_pubkey,
            config.validator.base_fees,
        );

        let ledger_resume_strategy = &config.ledger.resume_strategy();

        let (ledger, last_slot) = Self::init_ledger(&config.ledger)?;
        info!("Latest ledger slot: {}", last_slot);

        Self::sync_validator_keypair_with_ledger(
            ledger.ledger_path(),
            &identity_keypair,
            ledger_resume_strategy,
            config.ledger.skip_keypair_match_check,
        )?;

        // SAFETY:
        // this code will never panic as the ledger_path always appends the
        // rocksdb directory to whatever path is preconfigured for the ledger,
        // see `Ledger::do_open`, thus this path will always have a parent
        let storage_path = ledger
            .ledger_path()
            .parent()
            .expect("ledger_path didn't have a parent, should never happen");

        let latest_block = ledger.latest_block().load();
        let slot = ledger_resume_strategy.slot().unwrap_or(latest_block.slot);
        let accountsdb =
            AccountsDb::new(&config.accounts.db, storage_path, slot)?;
        for (pubkey, account) in genesis_config.accounts {
            accountsdb.insert_account(&pubkey, &account.into());
        }

        let exit = Arc::<AtomicBool>::default();
        let ledger_truncator = LedgerTruncator::new(
            ledger.clone(),
            DEFAULT_TRUNCATION_TIME_INTERVAL,
            config.ledger.size,
        );

        init_validator_identity(&accountsdb, &validator_pubkey);
        fund_magic_context(&accountsdb);
        fund_task_context(&accountsdb);

        let faucet_keypair =
            funded_faucet(&accountsdb, ledger.ledger_path().as_path())?;

        let metrics_config = &config.metrics;
        let accountsdb = Arc::new(accountsdb);

        let metrics = if metrics_config.enabled {
            let metrics_service =
                magicblock_metrics::try_start_metrics_service(
                    metrics_config.service.socket_addr(),
                    token.clone(),
                )
                .map_err(ApiError::FailedToStartMetricsService)?;

            let system_metrics_ticker = init_system_metrics_ticker(
                Duration::from_secs(
                    metrics_config.system_metrics_tick_interval_secs,
                ),
                &ledger,
                &accountsdb,
                token.clone(),
            );

            Some((metrics_service, system_metrics_ticker))
        } else {
            None
        };

        let accounts_config = try_get_remote_accounts_config(&config.accounts)?;

        let (dispatch, validator_channels) = link();

        let committor_persist_path =
            storage_path.join("committor_service.sqlite");
        debug!(
            "Committor service persists to: {}",
            committor_persist_path.display()
        );

        let committor_service = Self::init_committor_service(
            &identity_keypair,
            committor_persist_path,
            &accounts_config,
            &config.accounts.clone.prepare_lookup_tables,
        )
        .await?;
        let chainlink = Arc::new(
            Self::init_chainlink(
                committor_service.clone(),
                &accounts_config.remote_cluster,
                &config,
                &dispatch.transaction_scheduler,
                &ledger.latest_block().clone(),
                &accountsdb,
                validator_pubkey,
                faucet_keypair.pubkey(),
            )
            .await?,
        );

        let scheduled_commits_processor =
            committor_service.as_ref().map(|committor_service| {
                Arc::new(ScheduledCommitsProcessorImpl::new(
                    accountsdb.clone(),
                    committor_service.clone(),
                    chainlink.clone(),
                    dispatch.transaction_scheduler.clone(),
                ))
            });

        validator::init_validator_authority(identity_keypair);

        let txn_scheduler_state = TransactionSchedulerState {
            accountsdb: accountsdb.clone(),
            ledger: ledger.clone(),
            transaction_status_tx: validator_channels.transaction_status,
            txn_to_process_rx: validator_channels.transaction_to_process,
            account_update_tx: validator_channels.account_update,
            environment: build_svm_env(&accountsdb, latest_block.blockhash, 0),
        };
        txn_scheduler_state
            .load_upgradeable_programs(&programs_to_load(&config.programs))
            .map_err(|err| {
                ApiError::FailedToLoadProgramsIntoBank(format!("{:?}", err))
            })?;

        // Faucet keypair is only used for airdrops, which are not allowed in
        // the Ephemeral mode by setting the faucet to None in node context
        // (used by the RPC implementation), we effectively disable airdrops
        let faucet = (config.accounts.lifecycle != LifecycleMode::Ephemeral)
            .then_some(faucet_keypair);
        let node_context = NodeContext {
            identity: validator_pubkey,
            faucet,
            base_fee: config.validator.base_fees.unwrap_or_default(),
            featureset: txn_scheduler_state.environment.feature_set.clone(),
        };
        let transaction_scheduler =
            TransactionScheduler::new(1, txn_scheduler_state);
        transaction_scheduler.spawn();

        let shared_state = SharedState::new(
            node_context,
            accountsdb.clone(),
            ledger.clone(),
            chainlink.clone(),
            config.validator.millis_per_slot,
        );
        let rpc = JsonRpcServer::new(
            &config.rpc,
            shared_state,
            &dispatch,
            token.clone(),
        )
        .await?;
        let rpc_handle = tokio::spawn(rpc.run());

        Ok(Self {
            accountsdb,
            config,
            exit,
            _metrics: metrics,
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
            task_scheduler_handle: None,
        })
    }

    async fn init_committor_service(
        identity_keypair: &Keypair,
        committor_persist_path: PathBuf,
        accounts_config: &magicblock_accounts::AccountsConfig,
        prepare_lookup_tables: &PrepareLookupTables,
    ) -> ApiResult<Option<Arc<CommittorService>>> {
        // TODO(thlorenz): when we support lifecycle modes again, only start it when needed
        let committor_service = Some(Arc::new(CommittorService::try_start(
            identity_keypair.insecure_clone(),
            committor_persist_path,
            ChainConfig {
                rpc_uri: accounts_config.remote_cluster.url.clone(),
                commitment: CommitmentLevel::Confirmed,
                compute_budget_config: ComputeBudgetConfig::new(
                    accounts_config.commit_compute_unit_price,
                ),
            },
        )?));

        if let Some(committor_service) = &committor_service {
            if prepare_lookup_tables == &PrepareLookupTables::Always {
                debug!("Reserving common pubkeys for committor service");
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
    async fn init_chainlink(
        committor_service: Option<Arc<CommittorService>>,
        remote_cluster: &RemoteCluster,
        config: &EphemeralConfig,
        transaction_scheduler: &TransactionSchedulerHandle,
        latest_block: &LatestBlock,
        accountsdb: &Arc<AccountsDb>,
        validator_pubkey: Pubkey,
        faucet_pubkey: Pubkey,
    ) -> ApiResult<ChainlinkImpl> {
        use magicblock_chainlink::remote_account_provider::Endpoint;
        let rpc_url = remote_cluster.url.clone();
        let endpoints = remote_cluster
            .ws_urls
            .iter()
            .map(|pubsub_url| {
                Endpoint::new(rpc_url.clone(), pubsub_url.to_string())
            })
            .collect::<Vec<_>>();

        let cloner = ChainlinkCloner::new(
            committor_service,
            config.accounts.clone.clone(),
            transaction_scheduler.clone(),
            accountsdb.clone(),
            latest_block.clone(),
        );
        let cloner = Arc::new(cloner);
        let accounts_bank = accountsdb.clone();
        let chainlink_config = ChainlinkConfig::default_with_lifecycle_mode(
            LifecycleMode::Ephemeral.into(),
        );
        let commitment_config = {
            let level = CommitmentLevel::Confirmed;
            CommitmentConfig { commitment: level }
        };
        let chainlink = ChainlinkImpl::try_new_from_endpoints(
            &endpoints,
            commitment_config,
            &accounts_bank,
            &cloner,
            validator_pubkey,
            faucet_pubkey,
            chainlink_config,
        )
        .await?;

        Ok(chainlink)
    }

    fn init_ledger(
        ledger_config: &LedgerConfig,
    ) -> ApiResult<(Arc<Ledger>, Slot)> {
        let ledger_path = Path::new(&ledger_config.path);
        let (ledger, last_slot) = ledger::init(ledger_path, ledger_config)?;
        let ledger_shared = Arc::new(ledger);
        init_persister(ledger_shared.clone());
        Ok((ledger_shared, last_slot))
    }

    fn sync_validator_keypair_with_ledger(
        ledger_path: &Path,
        validator_keypair: &Keypair,
        resume_strategy: &LedgerResumeStrategy,
        skip_keypair_match_check: bool,
    ) -> ApiResult<()> {
        if !resume_strategy.is_resuming() {
            write_validator_keypair_to_ledger(ledger_path, validator_keypair)?;
        } else if let Ok(ledger_validator_keypair) =
            read_validator_keypair_from_ledger(ledger_path)
        {
            if ledger_validator_keypair.ne(validator_keypair)
                && !skip_keypair_match_check
            {
                return Err(
                    ApiError::LedgerValidatorKeypairNotMatchingProvidedKeypair(
                        ledger_path.display().to_string(),
                        ledger_validator_keypair.pubkey().to_string(),
                    ),
                );
            }
        } else {
            write_validator_keypair_to_ledger(ledger_path, validator_keypair)?;
        }

        Ok(())
    }

    // -----------------
    // Start/Stop
    // -----------------
    async fn maybe_process_ledger(&self) -> ApiResult<()> {
        if !self.config.ledger.resume_strategy().is_replaying() {
            return Ok(());
        }
        // SOLANA only allows blockhash to be valid for 150 slot back in time,
        // considering that the average slot time on solana is 400ms, then:
        const SOLANA_VALID_BLOCKHASH_AGE: u64 = 150 * 400;
        // we have this number for our max blockhash age in slots, which correspond to 60 seconds
        let max_block_age =
            SOLANA_VALID_BLOCKHASH_AGE / self.config.validator.millis_per_slot;
        let mut slot_to_continue_at = process_ledger(
            &self.ledger,
            &self.accountsdb,
            self.transaction_scheduler.clone(),
            max_block_age,
        )
        .await?;

        // The transactions to schedule and accept account commits re-run when we
        // process the ledger, however we do not want to re-commit them.
        // Thus while the ledger is processed we don't yet run the machinery to handle
        // scheduled commits and we clear all scheduled commits before fully starting the
        // validator.
        let scheduled_commits =
            ActionTransactionScheduler::default().scheduled_actions_len();
        debug!(
            "Found {} scheduled commits while processing ledger, clearing them",
            scheduled_commits
        );
        ActionTransactionScheduler::default().clear_scheduled_actions();

        // We want the next transaction either due to hydrating of cloned accounts or
        // user request to be processed in the next slot such that it doesn't become
        // part of the last block found in the existing ledger which would be incorrect.
        let (update_ledger_result, _) = advance_slot_and_update_ledger(
            &self.accountsdb,
            &self.ledger,
            &self.block_udpate_tx,
        );
        if let Err(err) = update_ledger_result {
            return Err(err.into());
        }
        if self.accountsdb.slot() != slot_to_continue_at {
            // NOTE: we used to return this error here, but this occurs very frequently
            // when running ledger restore integration tests, especially after
            // 6f52e376 (fix: sync accountsdb slot after ledger replay) was added.
            // It is a somewhat valid scenario in which the accounts db snapshot is more up to
            // date than the last ledger entry.
            // This means we lost some history, but our state is most up to date. In this case
            // we also don't need to replay anything.
            let err =
                ApiError::NextSlotAfterLedgerProcessingNotMatchingBankSlot(
                    slot_to_continue_at,
                    self.accountsdb.slot(),
                );
            warn!(
                "{err}, correcting to accoutns db slot {}",
                self.accountsdb.slot()
            );
            slot_to_continue_at = self.accountsdb.slot();
        }

        info!(
            "Processed ledger, validator continues at slot {}",
            slot_to_continue_at
        );

        Ok(())
    }

    async fn register_validator_on_chain(
        &self,
        fqdn: impl ToString,
    ) -> ApiResult<()> {
        let remote_cluster =
            remote_cluster_from_remote(&self.config.accounts.remote);
        let country_code =
            CountryCode::from(self.config.validator.country_code.alpha3());
        let validator_keypair = validator_authority();
        let validator_info = ErRecord::V0(RecordV0 {
            identity: validator_keypair.pubkey(),
            status: ErStatus::Active,
            block_time_ms: self.config.validator.millis_per_slot as u16,
            base_fee: self.config.validator.base_fees.unwrap_or(0) as u16,
            features: FeaturesSet::default(),
            load_average: 0, // not implemented
            country_code,
            addr: fqdn.to_string(),
        });

        DomainRegistryManager::handle_registration_static(
            remote_cluster.url,
            &validator_keypair,
            validator_info,
        )
        .map_err(|err| {
            ApiError::FailedToRegisterValidatorOnChain(format!("{:?}", err))
        })
    }

    fn unregister_validator_on_chain(&self) -> ApiResult<()> {
        let remote_cluster =
            remote_cluster_from_remote(&self.config.accounts.remote);
        let validator_keypair = validator_authority();

        DomainRegistryManager::handle_unregistration_static(
            remote_cluster.url,
            &validator_keypair,
        )
        .map_err(|err| {
            ApiError::FailedToUnregisterValidatorOnChain(format!("{err:#}"))
        })
    }

    async fn ensure_validator_funded_on_chain(&self) -> ApiResult<()> {
        // NOTE: 5 SOL seems reasonable, but we may require a different amount in the future
        const MIN_BALANCE_SOL: u64 = 5;
        let accounts_config =
            try_get_remote_accounts_config(&self.config.accounts)?;

        let lamports = RpcClient::new_with_commitment(
            accounts_config.remote_cluster.url.clone(),
            CommitmentConfig {
                commitment: CommitmentLevel::Confirmed,
            },
        )
        .get_balance(&self.identity)
        .await
        .map_err(|err| {
            ApiError::FailedToObtainValidatorOnChainBalance(
                self.identity,
                err.to_string(),
            )
        })?;
        if lamports < MIN_BALANCE_SOL * LAMPORTS_PER_SOL {
            Err(ApiError::ValidatorInsufficientlyFunded(
                self.identity,
                MIN_BALANCE_SOL,
            ))
        } else {
            Ok(())
        }
    }

    pub async fn start(&mut self) -> ApiResult<()> {
        if matches!(self.config.accounts.lifecycle, LifecycleMode::Ephemeral) {
            self.ensure_validator_funded_on_chain().await?;
            if let Some(ref fqdn) = self.config.validator.fqdn {
                self.register_validator_on_chain(fqdn).await?;
            }
        }

        // Ledger processing needs to happen before anything of the below
        self.maybe_process_ledger().await?;

        // Ledger replay has completed, we can now clean non-delegated accounts
        // including programs from the bank
        if !self
            .config
            .ledger
            .resume_strategy()
            .is_removing_accountsdb()
        {
            self.chainlink.reset_accounts_bank();
        }

        // Now we are ready to start all services and are ready to accept transactions
        let remote_cluster =
            remote_cluster_from_remote(&self.config.accounts.remote);
        self.claim_fees_task
            .start(self.config.clone(), remote_cluster.url);

        self.slot_ticker = Some(init_slot_ticker(
            self.accountsdb.clone(),
            &self.scheduled_commits_processor,
            self.ledger.clone(),
            Duration::from_millis(self.config.validator.millis_per_slot),
            self.transaction_scheduler.clone(),
            self.block_udpate_tx.clone(),
            self.exit.clone(),
        ));

        self.ledger_truncator.start();

        let task_scheduler_db_path =
            SchedulerDatabase::path(self.ledger.ledger_path().parent().expect(
                "ledger_path didn't have a parent, should never happen",
            ));
        debug!(
            "Task scheduler persists to: {}",
            task_scheduler_db_path.display()
        );
        let task_scheduler_handle = TaskSchedulerService::start(
            &task_scheduler_db_path,
            &self.config.task_scheduler,
            self.accountsdb.clone(),
            self.transaction_scheduler.clone(),
            self.ledger.latest_block().clone(),
            self.token.clone(),
        )?;
        // TODO: we should shutdown gracefully.
        // This is discussed in this comment:
        // https://github.com/magicblock-labs/magicblock-validator/pull/493#discussion_r2324560798
        // However there is no proper solution for this right now.
        // An issue to create a shutdown system is open here:
        // https://github.com/magicblock-labs/magicblock-validator/issues/524
        self.task_scheduler_handle = Some(tokio::spawn(async move {
            match task_scheduler_handle.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    error!("An error occurred while running the task scheduler: {:?}", err);
                    error!("Exiting process...");
                    std::process::exit(1);
                }
                Err(err) => {
                    error!("Failed to start task scheduler: {:?}", err);
                    error!("Exiting process...");
                    std::process::exit(1);
                }
            }
        }));

        validator::finished_starting_up();
        Ok(())
    }

    pub async fn stop(mut self) {
        self.exit.store(true, Ordering::Relaxed);

        // Ordering is important here
        // Commitor service shall be stopped last
        self.token.cancel();
        if let Some(ref scheduled_commits_processor) =
            self.scheduled_commits_processor
        {
            scheduled_commits_processor.stop();
        }
        if let Some(ref committor_service) = self.committor_service {
            committor_service.stop();
        }

        self.ledger_truncator.stop();
        self.claim_fees_task.stop();

        if self.config.validator.fqdn.is_some()
            && matches!(
                self.config.accounts.lifecycle,
                LifecycleMode::Ephemeral
            )
        {
            if let Err(err) = self.unregister_validator_on_chain() {
                error!("Failed to unregister: {}", err)
            }
        }
        self.accountsdb.flush();

        // we have two memory mapped databases, flush them to disk before exitting
        if let Err(err) = self.ledger.shutdown(false) {
            error!("Failed to shutdown ledger: {:?}", err);
        }
        let _ = self.rpc_handle.await;
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }
}

fn programs_to_load(programs: &[ProgramConfig]) -> Vec<(Pubkey, String)> {
    programs
        .iter()
        .map(|program| (program.id, program.path.clone()))
        .collect()
}

fn try_get_remote_accounts_config(
    accounts: &magicblock_config::AccountsConfig,
) -> ApiResult<magicblock_accounts::AccountsConfig> {
    try_convert_accounts_config(accounts).map_err(ApiError::ConfigError)
}
