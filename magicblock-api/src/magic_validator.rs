use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    thread,
    time::Duration,
};

use conjunto_transwise::RpcProviderConfig;
use log::*;
use magicblock_account_cloner::{
    map_committor_request_result, standard_blacklisted_accounts,
    CloneOutputMap, RemoteAccountClonerClient, RemoteAccountClonerWorker,
    ValidatorCollectionMode,
};
use magicblock_account_dumper::AccountDumperBank;
use magicblock_account_fetcher::{
    RemoteAccountFetcherClient, RemoteAccountFetcherWorker,
};
use magicblock_account_updates::{
    RemoteAccountUpdatesClient, RemoteAccountUpdatesWorker,
};
use magicblock_accounts::{
    utils::try_rpc_cluster_from_cluster, AccountsManager,
};
use magicblock_accounts_api::BankAccountProvider;
use magicblock_accounts_db::{
    config::AccountsDbConfig, error::AccountsDbError,
};
use magicblock_bank::{
    bank::Bank,
    genesis_utils::create_genesis_config_with_leader,
    geyser::{AccountsUpdateNotifier, TransactionNotifier},
    program_loader::load_programs_into_bank,
    transaction_logs::TransactionLogCollectorFilter,
};
use magicblock_committor_service::{
    config::ChainConfig, CommittorService, ComputeBudgetConfig,
};
use magicblock_config::{EphemeralConfig, LifecycleMode, ProgramConfig};
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use magicblock_ledger::{
    blockstore_processor::process_ledger,
    ledger_size_manager::{
        config::{
            ExistingLedgerState, LedgerSizeManagerConfig, ResizePercentage,
            CHECK_LEDGER_SIZE_INTERVAL_MS,
        },
        TruncatingLedgerSizeManager,
    },
    Ledger,
};
use magicblock_metrics::MetricsService;
use magicblock_perf_service::SamplePerformanceService;
use magicblock_processor::execute_transaction::TRANSACTION_INDEX_LOCK;
use magicblock_program::{
    init_persister, validator, validator::validator_authority,
};
use magicblock_pubsub::pubsub_service::{
    PubsubConfig, PubsubService, PubsubServiceCloseHandle,
};
use magicblock_rpc::{
    json_rpc_request_processor::JsonRpcConfig, json_rpc_service::JsonRpcService,
};
use magicblock_transaction_status::{
    TransactionStatusMessage, TransactionStatusSender,
};
use mdp::state::{
    features::FeaturesSet,
    record::{CountryCode, ErRecord},
    status::ErStatus,
    version::v0::RecordV0,
};
use solana_geyser_plugin_manager::{
    geyser_plugin_manager::GeyserPluginManager,
    slot_status_notifier::SlotStatusNotifierImpl,
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    clock::Slot,
    commitment_config::{CommitmentConfig, CommitmentLevel},
    genesis_config::GenesisConfig,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::{
    domain_registry_manager::DomainRegistryManager,
    errors::{ApiError, ApiResult},
    external_config::{cluster_from_remote, try_convert_accounts_config},
    fund_account::{
        fund_magic_context, fund_validator_identity, funded_faucet,
    },
    geyser_transaction_notify_listener::GeyserTransactionNotifyListener,
    init_geyser_service::{init_geyser_service, InitGeyserServiceConfig},
    ledger::{
        self, read_validator_keypair_from_ledger,
        write_validator_keypair_to_ledger,
    },
    slot::advance_slot_and_update_ledger,
    tickers::{
        init_commit_accounts_ticker, init_slot_ticker,
        init_system_metrics_ticker,
    },
};

// -----------------
// MagicValidatorConfig
// -----------------
#[derive(Default)]
pub struct MagicValidatorConfig {
    pub validator_config: EphemeralConfig,
    pub init_geyser_service_config: InitGeyserServiceConfig,
}

impl std::fmt::Debug for MagicValidatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MagicValidatorConfig")
            .field("validator_config", &self.validator_config)
            .field(
                "init_geyser_service_config",
                &self.init_geyser_service_config,
            )
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
    bank: Arc<Bank>,
    ledger: Arc<Ledger>,
    ledger_size_manager: TruncatingLedgerSizeManager,
    slot_ticker: Option<tokio::task::JoinHandle<()>>,
    pubsub_handle: RwLock<Option<thread::JoinHandle<()>>>,
    pubsub_close_handle: PubsubServiceCloseHandle,
    sample_performance_service: Option<SamplePerformanceService>,
    commit_accounts_ticker: Option<tokio::task::JoinHandle<()>>,
    remote_account_fetcher_worker: Option<RemoteAccountFetcherWorker>,
    remote_account_fetcher_handle: Option<tokio::task::JoinHandle<()>>,
    remote_account_updates_worker: Option<RemoteAccountUpdatesWorker>,
    remote_account_updates_handle: Option<tokio::task::JoinHandle<()>>,
    #[allow(clippy::type_complexity)]
    remote_account_cloner_worker: Option<
        Arc<
            RemoteAccountClonerWorker<
                BankAccountProvider,
                RemoteAccountFetcherClient,
                RemoteAccountUpdatesClient,
                AccountDumperBank,
                CommittorService,
            >,
        >,
    >,
    remote_account_cloner_handle: Option<tokio::task::JoinHandle<()>>,
    accounts_manager: Arc<AccountsManager>,
    committor_service: Option<Arc<CommittorService>>,
    transaction_listener: GeyserTransactionNotifyListener,
    rpc_service: JsonRpcService,
    _metrics: Option<(MetricsService, tokio::task::JoinHandle<()>)>,
    geyser_rpc_service: Arc<GeyserRpcService>,
    pubsub_config: PubsubConfig,
    pub transaction_status_sender: TransactionStatusSender,
}

impl MagicValidator {
    // -----------------
    // Initialization
    // -----------------
    pub fn try_from_config(
        config: MagicValidatorConfig,
        identity_keypair: Keypair,
    ) -> ApiResult<Self> {
        // TODO(thlorenz): this will need to be recreated on each start
        let token = CancellationToken::new();

        let (geyser_manager, geyser_rpc_service) =
            init_geyser_service(config.init_geyser_service_config)?;
        let geyser_manager = Arc::new(RwLock::new(geyser_manager));

        let validator_pubkey = identity_keypair.pubkey();
        let magicblock_bank::genesis_utils::GenesisConfigInfo {
            genesis_config,
            validator_pubkey,
            ..
        } = create_genesis_config_with_leader(
            u64::MAX,
            &validator_pubkey,
            config.validator_config.validator.base_fees,
        );

        let ledger = Self::init_ledger(
            config.validator_config.ledger.path.as_ref(),
            config.validator_config.ledger.reset,
        )?;
        Self::sync_validator_keypair_with_ledger(
            ledger.ledger_path(),
            &identity_keypair,
            config.validator_config.ledger.reset,
        )?;

        let existing_ledger_state = (!config.validator_config.ledger.reset
            && ledger.last_slot() > 0)
            .then_some(ExistingLedgerState {
                size: ledger.storage_size()?,
                slot: ledger.last_slot(),
                mod_id: ledger.last_mod_id(),
            });

        // SAFETY:
        // this code will never panic as the ledger_path always appends the
        // rocksdb directory to whatever path is preconfigured for the ledger,
        // see `Ledger::do_open`, thus this path will always have a parent
        let ledger_parent_path = ledger
            .ledger_path()
            .parent()
            .expect("ledger_path didn't have a parent, should never happen");

        let exit = Arc::<AtomicBool>::default();
        let bank = Self::init_bank(
            Some(geyser_manager.clone()),
            &genesis_config,
            &config.validator_config.accounts.db,
            config.validator_config.validator.millis_per_slot,
            validator_pubkey,
            ledger_parent_path,
            ledger.get_max_blockhash().map(|(slot, _)| slot)?,
        )?;

        fund_validator_identity(&bank, &validator_pubkey);
        fund_magic_context(&bank);
        let faucet_keypair = funded_faucet(
            &bank,
            ledger.ledger_path().as_path(),
            config.validator_config.ledger.reset,
        )?;

        load_programs_into_bank(
            &bank,
            &programs_to_load(&config.validator_config.programs),
        )
        .map_err(|err| {
            ApiError::FailedToLoadProgramsIntoBank(format!("{:?}", err))
        })?;

        let (transaction_sndr, transaction_listener) =
            Self::init_transaction_listener(
                &ledger,
                Some(TransactionNotifier::new(geyser_manager)),
            );

        let ledger_size_manager: TruncatingLedgerSizeManager =
            TruncatingLedgerSizeManager::new_from_ledger(
                ledger.clone(),
                bank.clone(),
                existing_ledger_state,
                LedgerSizeManagerConfig {
                    max_size: config.validator_config.ledger.size,
                    size_check_interval_ms: CHECK_LEDGER_SIZE_INTERVAL_MS,
                    resize_percentage: ResizePercentage::Large,
                },
            );

        let metrics_config = &config.validator_config.metrics;
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
                &bank,
                token.clone(),
            );

            Some((metrics_service, system_metrics_ticker))
        } else {
            None
        };

        let (accounts_config, remote_rpc_config) =
            try_get_remote_accounts_and_rpc_config(
                &config.validator_config.accounts,
            )?;

        let remote_account_fetcher_worker =
            RemoteAccountFetcherWorker::new(remote_rpc_config.clone());

        let remote_account_updates_worker = RemoteAccountUpdatesWorker::new(
            accounts_config.remote_cluster.ws_urls(),
            remote_rpc_config.commitment(),
            // We'll kill/refresh one connection every 50 minutes
            Duration::from_secs(60 * 50),
        );

        let transaction_status_sender = TransactionStatusSender {
            sender: transaction_sndr,
        };

        let bank_account_provider = BankAccountProvider::new(bank.clone());
        let remote_account_fetcher_client =
            RemoteAccountFetcherClient::new(&remote_account_fetcher_worker);
        let remote_account_updates_client =
            RemoteAccountUpdatesClient::new(&remote_account_updates_worker);
        let account_dumper_bank = AccountDumperBank::new(
            bank.clone(),
            Some(transaction_status_sender.clone()),
        );
        let blacklisted_accounts = standard_blacklisted_accounts(
            &identity_keypair.pubkey(),
            &faucet_keypair.pubkey(),
        );

        let committor_persist_path =
            ledger_parent_path.join("committor_service.sqlite");
        debug!(
            "Committor service persists to: {}",
            committor_persist_path.display()
        );

        let clone_permissions =
            accounts_config.lifecycle.to_account_cloner_permissions();
        let committor_service = if clone_permissions.can_clone() {
            Some(Arc::new(CommittorService::try_start(
                identity_keypair.insecure_clone(),
                committor_persist_path,
                ChainConfig {
                    rpc_uri: remote_rpc_config.url().to_string(),
                    commitment: remote_rpc_config
                        .commitment()
                        .unwrap_or(CommitmentLevel::Confirmed),
                    compute_budget_config: ComputeBudgetConfig::new(
                        accounts_config.commit_compute_unit_price,
                    ),
                },
            )?))
        } else {
            None
        };

        let remote_account_cloner_worker = RemoteAccountClonerWorker::new(
            bank_account_provider,
            remote_account_fetcher_client,
            remote_account_updates_client,
            account_dumper_bank,
            committor_service.clone(),
            accounts_config.allowed_program_ids,
            blacklisted_accounts,
            if config.validator_config.validator.base_fees.is_none() {
                ValidatorCollectionMode::NoFees
            } else {
                ValidatorCollectionMode::Fees
            },
            clone_permissions,
            identity_keypair.pubkey(),
            config.validator_config.accounts.max_monitored_accounts,
        );

        let accounts_manager = Self::init_accounts_manager(
            &bank,
            &remote_account_cloner_worker.get_last_clone_output(),
            RemoteAccountClonerClient::new(&remote_account_cloner_worker),
            transaction_status_sender.clone(),
            &identity_keypair,
            &config.validator_config,
        );

        let pubsub_config = PubsubConfig::from_rpc(
            config.validator_config.rpc.addr,
            config.validator_config.rpc.port,
            config.validator_config.rpc.max_ws_connections,
        );
        validator::init_validator_authority(identity_keypair);

        // Make sure we process the ledger before we're open to handle
        // transactions via RPC
        let rpc_service = Self::init_json_rpc_service(
            bank.clone(),
            ledger.clone(),
            faucet_keypair,
            &genesis_config,
            accounts_manager.clone(),
            transaction_status_sender.clone(),
            &pubsub_config,
            &config.validator_config,
        )?;

        Ok(Self {
            config: config.validator_config,
            exit,
            rpc_service,
            _metrics: metrics,
            geyser_rpc_service,
            slot_ticker: None,
            commit_accounts_ticker: None,
            remote_account_fetcher_worker: Some(remote_account_fetcher_worker),
            remote_account_fetcher_handle: None,
            remote_account_updates_worker: Some(remote_account_updates_worker),
            remote_account_updates_handle: None,
            remote_account_cloner_worker: Some(Arc::new(
                remote_account_cloner_worker,
            )),
            remote_account_cloner_handle: None,
            pubsub_handle: Default::default(),
            pubsub_close_handle: Default::default(),
            committor_service,
            sample_performance_service: None,
            pubsub_config,
            token,
            bank,
            ledger,
            ledger_size_manager,
            accounts_manager,
            transaction_listener,
            transaction_status_sender,
        })
    }

    fn init_bank(
        geyser_manager: Option<Arc<RwLock<GeyserPluginManager>>>,
        genesis_config: &GenesisConfig,
        accountsdb_config: &AccountsDbConfig,
        millis_per_slot: u64,
        validator_pubkey: Pubkey,
        adb_path: &Path,
        adb_init_slot: Slot,
    ) -> Result<Arc<Bank>, AccountsDbError> {
        let runtime_config = Default::default();
        let lock = TRANSACTION_INDEX_LOCK.clone();
        let bank = Bank::new(
            genesis_config,
            runtime_config,
            accountsdb_config,
            None,
            None,
            false,
            geyser_manager.clone().map(AccountsUpdateNotifier::new),
            geyser_manager.map(SlotStatusNotifierImpl::new),
            millis_per_slot,
            validator_pubkey,
            lock,
            adb_path,
            adb_init_slot,
        )?;
        bank.transaction_log_collector_config
            .write()
            .unwrap()
            .filter = TransactionLogCollectorFilter::All;
        Ok(Arc::new(bank))
    }

    fn init_accounts_manager(
        bank: &Arc<Bank>,
        cloned_accounts: &CloneOutputMap,
        remote_account_cloner_client: RemoteAccountClonerClient,
        transaction_status_sender: TransactionStatusSender,
        validator_keypair: &Keypair,
        config: &EphemeralConfig,
    ) -> Arc<AccountsManager> {
        let accounts_config = try_convert_accounts_config(&config.accounts)
            .expect(
            "Failed to derive accounts config from provided magicblock config",
        );
        let accounts_manager = AccountsManager::try_new(
            bank,
            cloned_accounts,
            remote_account_cloner_client,
            Some(transaction_status_sender),
            // NOTE: we could avoid passing a copy of the keypair here if we instead pass
            // something akin to a ValidatorTransactionSigner that gets it via the [validator_authority]
            // method from the [magicblock_program] module, forgetting it immediately after.
            // That way we would at least hold it in memory for a long time only in one place and in all other
            // places only temporarily
            validator_keypair.insecure_clone(),
            accounts_config,
        )
        .expect("Failed to create accounts manager");

        Arc::new(accounts_manager)
    }

    #[allow(clippy::too_many_arguments)]
    fn init_json_rpc_service(
        bank: Arc<Bank>,
        ledger: Arc<Ledger>,
        faucet_keypair: Keypair,
        genesis_config: &GenesisConfig,
        accounts_manager: Arc<AccountsManager>,
        transaction_status_sender: TransactionStatusSender,
        pubsub_config: &PubsubConfig,
        config: &EphemeralConfig,
    ) -> ApiResult<JsonRpcService> {
        let rpc_socket_addr = SocketAddr::new(config.rpc.addr, config.rpc.port);
        let rpc_json_config = JsonRpcConfig {
            slot_duration: Duration::from_millis(
                config.validator.millis_per_slot,
            ),
            genesis_creation_time: genesis_config.creation_time,
            transaction_status_sender: Some(transaction_status_sender.clone()),
            rpc_socket_addr: Some(rpc_socket_addr),
            pubsub_socket_addr: Some(*pubsub_config.socket()),
            enable_rpc_transaction_history: true,
            disable_sigverify: !config.validator.sigverify,

            ..Default::default()
        };

        JsonRpcService::try_init(
            bank,
            ledger.clone(),
            faucet_keypair,
            genesis_config.hash(),
            accounts_manager,
            rpc_json_config,
        )
        .map_err(|err| {
            ApiError::FailedToInitJsonRpcService(format!("{:?}", err))
        })
    }

    fn init_ledger(
        ledger_path: Option<&String>,
        reset: bool,
    ) -> ApiResult<Arc<Ledger>> {
        let ledger_path = match ledger_path {
            Some(ledger_path) => PathBuf::from(ledger_path),
            None => {
                let ledger_path = TempDir::new()?;
                ledger_path.path().to_path_buf()
            }
        };
        let ledger = ledger::init(ledger_path, reset)?;
        let ledger_shared = Arc::new(ledger);
        init_persister(ledger_shared.clone());
        Ok(ledger_shared)
    }

    fn sync_validator_keypair_with_ledger(
        ledger_path: &Path,
        validator_keypair: &Keypair,
        reset_ledger: bool,
    ) -> ApiResult<()> {
        if reset_ledger {
            write_validator_keypair_to_ledger(ledger_path, validator_keypair)?;
        } else {
            let ledger_validator_keypair =
                read_validator_keypair_from_ledger(ledger_path)?;
            if ledger_validator_keypair.ne(validator_keypair) {
                return Err(
                    ApiError::LedgerValidatorKeypairNotMatchingProvidedKeypair(
                        ledger_path.display().to_string(),
                        ledger_validator_keypair.pubkey().to_string(),
                    ),
                );
            }
        }
        Ok(())
    }

    fn init_transaction_listener(
        ledger: &Arc<Ledger>,
        transaction_notifier: Option<TransactionNotifier>,
    ) -> (
        crossbeam_channel::Sender<TransactionStatusMessage>,
        GeyserTransactionNotifyListener,
    ) {
        let (transaction_sndr, transaction_recvr) =
            crossbeam_channel::unbounded();
        (
            transaction_sndr,
            GeyserTransactionNotifyListener::new(
                transaction_notifier,
                transaction_recvr,
                ledger.clone(),
            ),
        )
    }

    // -----------------
    // Start/Stop
    // -----------------
    fn maybe_process_ledger(&self) -> ApiResult<()> {
        if self.config.ledger.reset {
            return Ok(());
        }
        let slot_to_continue_at = process_ledger(&self.ledger, &self.bank)?;

        // The transactions to schedule and accept account commits re-run when we
        // process the ledger, however we do not want to re-commit them.
        // Thus while the ledger is processed we don't yet run the machinery to handle
        // scheduled commits and we clear all scheduled commits before fully starting the
        // validator.
        let scheduled_commits = self.accounts_manager.scheduled_commits_len();
        debug!(
            "Found {} scheduled commits while processing ledger, clearing them",
            scheduled_commits
        );
        self.accounts_manager.clear_scheduled_commits();

        // We want the next transaction either due to hydrating of cloned accounts or
        // user request to be processed in the next slot such that it doesn't become
        // part of the last block found in the existing ledger which would be incorrect.
        let (update_ledger_result, _) =
            advance_slot_and_update_ledger(&self.bank, &self.ledger);
        if let Err(err) = update_ledger_result {
            return Err(err.into());
        }
        if self.bank.slot() != slot_to_continue_at {
            return Err(
                ApiError::NextSlotAfterLedgerProcessingNotMatchingBankSlot(
                    slot_to_continue_at,
                    self.bank.slot(),
                ),
            );
        }

        info!(
            "Processed ledger, validator continues at slot {}",
            slot_to_continue_at
        );

        Ok(())
    }

    async fn register_validator_on_chain(
        &self,
        fdqn: impl ToString,
    ) -> ApiResult<()> {
        let url = cluster_from_remote(&self.config.accounts.remote);
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
            addr: fdqn.to_string(),
        });

        DomainRegistryManager::handle_registration_static(
            url.url(),
            &validator_keypair,
            validator_info,
        )
        .map_err(|err| {
            ApiError::FailedToRegisterValidatorOnChain(format!("{:?}", err))
        })
    }

    fn unregister_validator_on_chain(&self) -> ApiResult<()> {
        let url = cluster_from_remote(&self.config.accounts.remote);
        let validator_keypair = validator_authority();

        DomainRegistryManager::handle_unregistration_static(
            url.url(),
            &validator_keypair,
        )
        .map_err(|err| {
            ApiError::FailedToUnregisterValidatorOnChain(format!("{err:#}"))
        })
    }

    async fn ensure_validator_funded_on_chain(&self) -> ApiResult<()> {
        // NOTE: 5 SOL seems reasonable, but we may require a different amount in the future
        const MIN_BALANCE_SOL: u64 = 5;
        let (_, remote_rpc_config) =
            try_get_remote_accounts_and_rpc_config(&self.config.accounts)?;
        let validator_pubkey = self.bank().get_identity();

        let lamports = RpcClient::new_with_commitment(
            remote_rpc_config.url().to_string(),
            CommitmentConfig {
                commitment: remote_rpc_config
                    .commitment()
                    .unwrap_or(CommitmentLevel::Confirmed),
            },
        )
        .get_balance(&validator_pubkey)
        .await
        .map_err(|err| {
            ApiError::FailedToObtainValidatorOnChainBalance(
                validator_pubkey,
                err.to_string(),
            )
        })?;
        if lamports < MIN_BALANCE_SOL * LAMPORTS_PER_SOL {
            Err(ApiError::ValidatorInsufficientlyFunded(
                validator_pubkey,
                MIN_BALANCE_SOL,
            ))
        } else {
            Ok(())
        }
    }

    pub async fn start(&mut self) -> ApiResult<()> {
        if matches!(self.config.accounts.lifecycle, LifecycleMode::Ephemeral) {
            self.ensure_validator_funded_on_chain().await?;
            if let Some(ref fdqn) = self.config.validator.fdqn {
                self.register_validator_on_chain(fdqn).await?;
            }
        }

        self.maybe_process_ledger()?;

        self.transaction_listener.run(true, self.bank.clone());

        self.slot_ticker = Some(init_slot_ticker(
            &self.bank,
            &self.accounts_manager,
            self.committor_service.clone(),
            Some(self.transaction_status_sender.clone()),
            self.ledger.clone(),
            Duration::from_millis(self.config.validator.millis_per_slot),
            self.exit.clone(),
        ));

        self.commit_accounts_ticker = Some(init_commit_accounts_ticker(
            &self.accounts_manager,
            Duration::from_millis(self.config.accounts.commit.frequency_millis),
            self.token.clone(),
        ));

        // NOTE: these need to startup in the right order, otherwise some worker
        //       that may be needed, i.e. during hydration after ledger replay
        //       are not started in time
        self.start_remote_account_fetcher_worker();
        self.start_remote_account_updates_worker();
        self.start_remote_account_cloner_worker().await?;

        self.ledger_size_manager.try_start()?;

        self.rpc_service.start().map_err(|err| {
            ApiError::FailedToStartJsonRpcService(format!("{:?}", err))
        })?;

        info!(
            "Launched JSON RPC service at {:?} as part of process with pid {}",
            self.rpc_service.rpc_addr(),
            process::id(),
        );

        // NOTE: we need to create the pubsub service on each start since spawning
        // it takes ownership
        let pubsub_service = PubsubService::new(
            self.pubsub_config.clone(),
            self.geyser_rpc_service.clone(),
            self.bank.clone(),
        );

        let (pubsub_handle, pubsub_close_handle) =
            pubsub_service.spawn(self.pubsub_config.socket())?;
        self.pubsub_handle.write().unwrap().replace(pubsub_handle);
        self.pubsub_close_handle = pubsub_close_handle;

        self.sample_performance_service
            .replace(SamplePerformanceService::new(
                &self.bank,
                &self.ledger,
                self.exit.clone(),
            ));

        validator::finished_starting_up();
        Ok(())
    }

    fn start_remote_account_fetcher_worker(&mut self) {
        if let Some(mut remote_account_fetcher_worker) =
            self.remote_account_fetcher_worker.take()
        {
            let cancellation_token = self.token.clone();
            self.remote_account_fetcher_handle =
                Some(tokio::spawn(async move {
                    remote_account_fetcher_worker
                        .start_fetch_request_processing(cancellation_token)
                        .await;
                }));
        }
    }

    fn start_remote_account_updates_worker(&mut self) {
        if let Some(mut remote_account_updates_worker) =
            self.remote_account_updates_worker.take()
        {
            let cancellation_token = self.token.clone();
            self.remote_account_updates_handle =
                Some(tokio::spawn(async move {
                    remote_account_updates_worker
                        .start_monitoring_request_processing(cancellation_token)
                        .await
                }));
        }
    }

    async fn start_remote_account_cloner_worker(&mut self) -> ApiResult<()> {
        if let Some(remote_account_cloner_worker) =
            self.remote_account_cloner_worker.take()
        {
            if let Some(committor_service) = self.committor_service.as_ref() {
                debug!("Reserving common pubkeys for committor service");
                map_committor_request_result(
                    committor_service.reserve_common_pubkeys(),
                    committor_service.clone(),
                )
                .await?;
            }

            if !self.config.ledger.reset {
                let remote_account_cloner_worker =
                    remote_account_cloner_worker.clone();
                tokio::spawn(async move {
                    let _ = remote_account_cloner_worker
                        .hydrate()
                        .await
                        .inspect_err(|err| {
                            error!(
                                "Failed to hydrate validator accounts: {:?}",
                                err
                            );
                        });
                    info!("Validator hydration complete (bank hydrate, replay, account clone)");
                });
            }

            let cancellation_token = self.token.clone();
            self.remote_account_cloner_handle =
                Some(tokio::spawn(async move {
                    remote_account_cloner_worker
                        .start_clone_request_processing(cancellation_token)
                        .await
                }));
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        self.exit.store(true, Ordering::Relaxed);
        self.rpc_service.close();
        PubsubService::close(&self.pubsub_close_handle);
        self.token.cancel();
        self.ledger_size_manager.stop();

        // wait a bit for services to stop
        thread::sleep(Duration::from_secs(1));

        if self.config.validator.fdqn.is_some()
            && matches!(
                self.config.accounts.lifecycle,
                LifecycleMode::Ephemeral
            )
        {
            if let Err(err) = self.unregister_validator_on_chain() {
                error!("Failed to unregister: {}", err)
            }
        }

        // we have two memory mapped databases, flush them to disk before exitting
        self.bank.flush();
        if let Err(err) = self.ledger.shutdown(false) {
            error!("Failed to shutdown ledger: {:?}", err);
        }
    }

    pub fn join(self) {
        self.rpc_service.join().unwrap();
        if let Some(x) = self.pubsub_handle.write().unwrap().take() {
            x.join().unwrap()
        }
    }

    pub fn bank_rc(&self) -> Arc<Bank> {
        self.bank.clone()
    }

    pub fn bank(&self) -> &Bank {
        &self.bank
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

fn try_get_remote_accounts_and_rpc_config(
    accounts: &magicblock_config::AccountsConfig,
) -> ApiResult<(magicblock_accounts::AccountsConfig, RpcProviderConfig)> {
    let accounts_config =
        try_convert_accounts_config(accounts).map_err(ApiError::ConfigError)?;
    let remote_rpc_config = RpcProviderConfig::new(
        try_rpc_cluster_from_cluster(&accounts_config.remote_cluster)?,
        Some(CommitmentLevel::Confirmed),
    );
    Ok((accounts_config, remote_rpc_config))
}
