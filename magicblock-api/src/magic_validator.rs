use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};

use light_client::indexer::photon_indexer::PhotonIndexer;
use log::*;
use magicblock_account_cloner::{
    map_committor_request_result, ChainlinkCloner,
};
use magicblock_accounts::{
    scheduled_commits_processor::ScheduledCommitsProcessorImpl,
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
        chain_pubsub_client::ChainPubsubClientImpl,
        chain_rpc_client::ChainRpcClientImpl, photon_client::PhotonClientImpl,
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
use tokio::runtime::Builder;
use tokio_util::sync::CancellationToken;

use crate::{
    domain_registry_manager::DomainRegistryManager,
    errors::{ApiError, ApiResult},
    fund_account::{
        fund_magic_context, funded_faucet, init_validator_identity,
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
    SubMuxClient<ChainPubsubClientImpl>,
    AccountsDb,
    ChainlinkCloner,
    PhotonClientImpl,
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
}

impl MagicValidator {
    // -----------------
    // Initialization
    // -----------------
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

        let (ledger, last_slot) =
            Self::init_ledger(&config.ledger, &config.storage)?;
        info!("Latest ledger slot: {}", last_slot);
        let ledger_path = ledger.ledger_path();

        Self::sync_validator_keypair_with_ledger(
            ledger_path,
            &identity_keypair,
            config.ledger.verify_keypair,
        )?;

        let latest_block = ledger.latest_block().load();
        let accountsdb =
            AccountsDb::new(&config.accountsdb, &config.storage, last_slot)?;
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

        let faucet_keypair =
            funded_faucet(&accountsdb, ledger.ledger_path().as_path())?;

        let accountsdb = Arc::new(accountsdb);

        let metrics_service = magicblock_metrics::try_start_metrics_service(
            config.metrics.address.0,
            token.clone(),
        )
        .map_err(ApiError::FailedToStartMetricsService)?;

        let system_metrics_ticker = init_system_metrics_ticker(
            config.metrics.collect_frequency,
            &ledger,
            &accountsdb,
            token.clone(),
        );

        let (mut dispatch, validator_channels) = link();

        let committor_service = Self::init_committor_service(&config).await?;
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
        };
        TRANSACTION_COUNT.inc_by(ledger.count_transactions()? as u64);
        txn_scheduler_state
            .load_upgradeable_programs(&programs_to_load(&config.programs))
            .map_err(|err| {
                ApiError::FailedToLoadProgramsIntoBank(format!("{:?}", err))
            })?;

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
        let transaction_scheduler =
            TransactionScheduler::new(1, txn_scheduler_state);
        transaction_scheduler.spawn();

        let shared_state = SharedState::new(
            node_context,
            accountsdb.clone(),
            ledger.clone(),
            chainlink.clone(),
        );
        let rpc = JsonRpcServer::new(
            config.listen,
            shared_state,
            &dispatch,
            token.clone(),
        )
        .await?;
        let rpc_handle = thread::spawn(move || {
            let workers = (num_cpus::get() / 2 - 1).max(1);
            let runtime = Builder::new_multi_thread()
                .worker_threads(workers)
                .enable_all()
                .thread_name("rpc-worker")
                .build()
                .expect("failed to bulid async runtime for rpc service");
            runtime.block_on(rpc.run());
        });

        let task_scheduler_db_path =
            SchedulerDatabase::path(ledger.ledger_path().parent().expect(
                "ledger_path didn't have a parent, should never happen",
            ));
        debug!(
            "Task scheduler persists to: {}",
            task_scheduler_db_path.display()
        );
        let task_scheduler = TaskSchedulerService::new(
            &task_scheduler_db_path,
            &config.task_scheduler,
            dispatch.transaction_scheduler.clone(),
            dispatch
                .tasks_service
                .take()
                .expect("tasks_service should be initialized"),
            ledger.latest_block().clone(),
            token.clone(),
        )?;

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
        })
    }

    async fn init_committor_service(
        config: &ValidatorParams,
    ) -> ApiResult<Option<Arc<CommittorService>>> {
        let photon_client = config.compression.photon_url.as_ref().map(|url| {
            Arc::new(PhotonIndexer::new(
                url.clone(),
                config.compression.api_key.clone(),
            ))
        });

        let committor_persist_path =
            config.storage.join("committor_service.sqlite");
        debug!(
            "Committor service persists to: {}",
            committor_persist_path.display()
        );
        // TODO(thlorenz): when we support lifecycle modes again, only start it when needed
        let committor_service = Some(Arc::new(CommittorService::try_start(
            config.validator.keypair.insecure_clone(),
            committor_persist_path,
            ChainConfig {
                rpc_uri: config.remote.http().to_string(),
                commitment: CommitmentLevel::Confirmed,
                compute_budget_config: ComputeBudgetConfig::new(
                    config.commit.compute_unit_price,
                ),
            },
            photon_client,
        )?));

        if let Some(committor_service) = &committor_service {
            if config.chainlink.prepare_lookup_tables {
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
        config: &ValidatorParams,
        committor_service: Option<Arc<CommittorService>>,
        transaction_scheduler: &TransactionSchedulerHandle,
        latest_block: &LatestBlock,
        accountsdb: &Arc<AccountsDb>,
        faucet_pubkey: Pubkey,
    ) -> ApiResult<ChainlinkImpl> {
        use magicblock_chainlink::remote_account_provider::Endpoint;
        let rpc_url = config.remote.http().to_string();
        let mut endpoints = config
            .remote
            .websocket()
            .map(|pubsub_url| Endpoint::Rpc {
                rpc_url: rpc_url.clone(),
                pubsub_url: pubsub_url.to_string(),
            })
            .collect::<Vec<_>>();

        if let Some(url) = config.compression.photon_url.as_ref() {
            endpoints.push(Endpoint::Compression { url: url.clone() });
        }

        let cloner = ChainlinkCloner::new(
            committor_service,
            config.chainlink.clone(),
            transaction_scheduler.clone(),
            accountsdb.clone(),
            latest_block.clone(),
        );
        let cloner = Arc::new(cloner);
        let accounts_bank = accountsdb.clone();
        let mut chainlink_config = ChainlinkConfig::default_with_lifecycle_mode(
            LifecycleMode::Ephemeral,
        );
        chainlink_config.remove_confined_accounts =
            config.chainlink.remove_confined_accounts;
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
        init_persister(ledger_shared.clone());
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
            warn!("Skipping ledger keypair verification due to configuration");
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
        let slot_to_continue_at = process_ledger(
            &self.ledger,
            accountsdb_slot,
            self.transaction_scheduler.clone(),
            max_block_age,
        )
        .await?;
        self.accountsdb.set_slot(slot_to_continue_at);

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

        info!(
            "Processed ledger, validator continues at slot {}",
            slot_to_continue_at
        );

        Ok(())
    }

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
            self.config.remote.http(),
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
            self.config.remote.http(),
            &validator_keypair,
        )
        .map_err(|err| {
            ApiError::FailedToUnregisterValidatorOnChain(format!("{err:#}"))
        })
    }

    async fn ensure_validator_funded_on_chain(&self) -> ApiResult<()> {
        // NOTE: 5 SOL seems reasonable, but we may require a different amount in the future
        const MIN_BALANCE_SOL: u64 = 5;

        let lamports = RpcClient::new_with_commitment(
            self.config.remote.http().to_string(),
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
        if matches!(self.config.lifecycle, LifecycleMode::Ephemeral) {
            self.ensure_validator_funded_on_chain().await?;
            if let Some(ref config) = self.config.chain_operation {
                self.register_validator_on_chain(config).await?;
            }
        }

        // Ledger processing needs to happen before anything of the below
        self.maybe_process_ledger().await?;

        // Ledger replay has completed, we can now clean non-delegated accounts
        // including programs from the bank
        if !self.config.accountsdb.reset {
            self.chainlink.reset_accounts_bank();
        }

        // Now we are ready to start all services and are ready to accept transactions
        if let Some(frequency) = self
            .config
            .chain_operation
            .as_ref()
            .filter(|co| !co.claim_fees_frequency.is_zero())
            .map(|co| co.claim_fees_frequency)
        {
            self.claim_fees_task
                .start(frequency, self.config.remote.http().to_string());
        }

        self.slot_ticker = Some(init_slot_ticker(
            self.accountsdb.clone(),
            &self.scheduled_commits_processor,
            self.ledger.clone(),
            self.config.ledger.block_time,
            self.transaction_scheduler.clone(),
            self.block_udpate_tx.clone(),
            self.exit.clone(),
        ));

        self.ledger_truncator.start();

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
            let join_handle = match task_scheduler.start().await {
                Ok(join_handle) => join_handle,
                Err(err) => {
                    error!("Failed to start task scheduler: {:?}", err);
                    error!("Exiting process...");
                    std::process::exit(1);
                }
            };
            match join_handle.await {
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
        });

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

        if self.config.chain_operation.is_some()
            && matches!(self.config.lifecycle, LifecycleMode::Ephemeral)
        {
            if let Err(err) = self.unregister_validator_on_chain() {
                error!("Failed to unregister: {}", err)
            }
        }
        self.accountsdb.flush();

        // we have two memory mapped databases, flush them to disk before exitting
        if let Err(err) = self.ledger.shutdown(true) {
            error!("Failed to shutdown ledger: {:?}", err);
        }
        let _ = self.rpc_handle.join();
        if let Err(err) = self.ledger_truncator.join() {
            error!("Ledger truncator did not gracefully exit: {:?}", err);
        }
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }
}

fn programs_to_load(programs: &[LoadableProgram]) -> Vec<(Pubkey, PathBuf)> {
    programs
        .iter()
        .map(|program| (program.id.0, program.path.clone()))
        .collect()
}
