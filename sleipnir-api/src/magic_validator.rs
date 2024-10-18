use std::{
    net::SocketAddr,
    path::PathBuf,
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
use sleipnir_account_cloner::{
    standard_blacklisted_accounts, RemoteAccountClonerClient,
    RemoteAccountClonerWorker,
};
use sleipnir_account_dumper::AccountDumperBank;
use sleipnir_account_fetcher::{
    RemoteAccountFetcherClient, RemoteAccountFetcherWorker,
};
use sleipnir_account_updates::{
    RemoteAccountUpdatesClient, RemoteAccountUpdatesWorker,
};
use sleipnir_accounts::{utils::try_rpc_cluster_from_cluster, AccountsManager};
use sleipnir_accounts_api::BankAccountProvider;
use sleipnir_bank::{
    bank::Bank, genesis_utils::create_genesis_config_with_leader,
    program_loader::load_programs_into_bank,
    transaction_logs::TransactionLogCollectorFilter,
    transaction_notifier_interface::TransactionNotifierArc,
};
use sleipnir_config::{ProgramConfig, SleipnirConfig};
use sleipnir_geyser_plugin::rpc::GeyserRpcService;
use sleipnir_ledger::Ledger;
use sleipnir_metrics::MetricsService;
use sleipnir_perf_service::SamplePerformanceService;
use sleipnir_program::init_validator_authority;
use sleipnir_pubsub::pubsub_service::{
    PubsubConfig, PubsubService, PubsubServiceCloseHandle,
};
use sleipnir_rpc::{
    json_rpc_request_processor::JsonRpcConfig, json_rpc_service::JsonRpcService,
};
use sleipnir_transaction_status::{
    TransactionStatusMessage, TransactionStatusSender,
};
use solana_geyser_plugin_manager::geyser_plugin_service::GeyserPluginService;
use solana_sdk::{
    commitment_config::CommitmentLevel, genesis_config::GenesisConfig,
    pubkey::Pubkey, signature::Keypair, signer::Signer,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::{
    errors::{ApiError, ApiResult},
    external_config::try_convert_accounts_config,
    fund_account::{
        fund_magic_context, fund_validator_identity, funded_faucet,
    },
    geyser_transaction_notify_listener::GeyserTransactionNotifyListener,
    init_geyser_service::{init_geyser_service, InitGeyserServiceConfig},
    ledger,
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
    pub validator_config: SleipnirConfig,
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
    config: SleipnirConfig,
    exit: Arc<AtomicBool>,
    token: CancellationToken,
    bank: Arc<Bank>,
    ledger: Arc<Ledger>,
    slot_ticker: Option<tokio::task::JoinHandle<()>>,
    pubsub_handle: RwLock<Option<thread::JoinHandle<()>>>,
    pubsub_close_handle: PubsubServiceCloseHandle,
    sample_performance_service: Option<SamplePerformanceService>,
    commit_accounts_ticker: Option<tokio::task::JoinHandle<()>>,
    remote_account_fetcher_worker: Option<RemoteAccountFetcherWorker>,
    remote_account_fetcher_handle: Option<thread::JoinHandle<()>>,
    remote_account_updates_worker: Option<RemoteAccountUpdatesWorker>,
    remote_account_updates_handle: Option<thread::JoinHandle<()>>,
    remote_account_cloner_worker: Option<
        RemoteAccountClonerWorker<
            BankAccountProvider,
            RemoteAccountFetcherClient,
            RemoteAccountUpdatesClient,
            AccountDumperBank,
        >,
    >,
    remote_account_cloner_handle: Option<thread::JoinHandle<()>>,
    accounts_manager: Arc<AccountsManager>,
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
        // TODO(thlorenz): @@ this will need to be recreated on each start
        let token = CancellationToken::new();

        let (geyser_service, geyser_rpc_service) =
            init_geyser_service(config.init_geyser_service_config)?;

        let validator_pubkey = identity_keypair.pubkey();
        let sleipnir_bank::genesis_utils::GenesisConfigInfo {
            genesis_config,
            validator_pubkey,
            ..
        } = create_genesis_config_with_leader(u64::MAX, &validator_pubkey);

        let exit = Arc::<AtomicBool>::default();
        let bank = Self::init_bank(
            &geyser_service,
            &genesis_config,
            config.validator_config.validator.millis_per_slot,
            validator_pubkey,
        );

        let ledger = Self::init_ledger(
            config.validator_config.ledger.path.as_ref(),
            config.validator_config.ledger.reset,
        )?;

        fund_validator_identity(&bank, &validator_pubkey);
        fund_magic_context(&bank);
        let faucet_keypair = funded_faucet(&bank);

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
                geyser_service.get_transaction_notifier(),
            );

        let metrics_config = &config.validator_config.metrics;
        let metrics = if metrics_config.enabled {
            let metrics_service = sleipnir_metrics::try_start_metrics_service(
                metrics_config.service.socket_addr(),
                token.clone(),
            )
            .map_err(ApiError::FailedToStartMetricsService)?;

            let system_metrics_ticker = init_system_metrics_ticker(
                Duration::from_secs(
                    metrics_config.system_metrics_tick_interval_secs,
                ),
                &ledger,
                token.clone(),
            );

            Some((metrics_service, system_metrics_ticker))
        } else {
            None
        };

        let accounts_config =
            try_convert_accounts_config(&config.validator_config.accounts)
                .map_err(ApiError::ConfigError)?;

        let remote_rpc_config = RpcProviderConfig::new(
            try_rpc_cluster_from_cluster(&accounts_config.remote_cluster)?,
            Some(CommitmentLevel::Confirmed),
        );

        let remote_account_fetcher_worker =
            RemoteAccountFetcherWorker::new(remote_rpc_config.clone());

        let remote_account_updates_worker = RemoteAccountUpdatesWorker::new(
            // We'll maintain 3 connections constantly (those could be on different nodes if we wanted to)
            vec![
                remote_rpc_config.clone(),
                remote_rpc_config.clone(),
                remote_rpc_config.clone(),
            ],
            // We'll kill/refresh one connection every 5 minutes
            Duration::from_secs(60 * 5),
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
        let blacklisted_accounts =
            standard_blacklisted_accounts(&identity_keypair.pubkey());

        let remote_account_cloner_worker = RemoteAccountClonerWorker::new(
            bank_account_provider,
            remote_account_fetcher_client,
            remote_account_updates_client,
            account_dumper_bank,
            accounts_config.allowed_program_ids,
            blacklisted_accounts,
            accounts_config.payer_init_lamports,
            accounts_config.lifecycle.to_account_cloner_permissions(),
        );

        let accounts_manager = Self::init_accounts_manager(
            &bank,
            RemoteAccountClonerClient::new(&remote_account_cloner_worker),
            transaction_status_sender.clone(),
            &identity_keypair,
            &config.validator_config,
        );

        let pubsub_config = PubsubConfig::from_rpc(
            config.validator_config.rpc.addr,
            config.validator_config.rpc.port,
        );
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

        init_validator_authority(identity_keypair);

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
            remote_account_cloner_worker: Some(remote_account_cloner_worker),
            remote_account_cloner_handle: None,
            pubsub_handle: Default::default(),
            pubsub_close_handle: Default::default(),
            sample_performance_service: None,
            pubsub_config,
            token,
            bank,
            ledger,
            accounts_manager,
            transaction_listener,
            transaction_status_sender,
        })
    }

    fn init_bank(
        geyser_service: &GeyserPluginService,
        genesis_config: &GenesisConfig,
        millis_per_slot: u64,
        validator_pubkey: Pubkey,
    ) -> Arc<Bank> {
        let runtime_config = Default::default();
        let bank = Bank::new(
            genesis_config,
            runtime_config,
            None,
            None,
            false,
            geyser_service.get_accounts_update_notifier(),
            geyser_service.get_slot_status_notifier(),
            millis_per_slot,
            validator_pubkey,
        );
        bank.transaction_log_collector_config
            .write()
            .unwrap()
            .filter = TransactionLogCollectorFilter::All;
        Arc::new(bank)
    }

    fn init_accounts_manager(
        bank: &Arc<Bank>,
        remote_account_cloner_client: RemoteAccountClonerClient,
        transaction_status_sender: TransactionStatusSender,
        validator_keypair: &Keypair,
        config: &SleipnirConfig,
    ) -> Arc<AccountsManager> {
        let accounts_config = try_convert_accounts_config(&config.accounts)
            .expect(
            "Failed to derive accounts config from provided sleipnir config",
        );
        let accounts_manager = AccountsManager::try_new(
            bank,
            remote_account_cloner_client,
            Some(transaction_status_sender),
            // NOTE: we could avoid passing a copy of the keypair here if we instead pass
            // something akin to a ValidatorTransactionSigner that gets it via the [validator_authority]
            // method from the [sleipnir_program] module, forgetting it immediately after.
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
        config: &SleipnirConfig,
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
        Ok(Arc::new(ledger))
    }

    fn init_transaction_listener(
        ledger: &Arc<Ledger>,
        transaction_notifier: Option<TransactionNotifierArc>,
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
    pub async fn start(&mut self) -> ApiResult<()> {
        // NOE: this only run only once, i.e. at creation time
        self.transaction_listener.run(true);

        self.slot_ticker = Some(init_slot_ticker(
            &self.bank,
            &self.accounts_manager,
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

        self.start_remote_account_fetcher_worker();
        self.start_remote_account_updates_worker();
        self.start_remote_account_cloner_worker();

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

        Ok(())
    }

    fn start_remote_account_fetcher_worker(&mut self) {
        if let Some(mut remote_account_fetcher_worker) =
            self.remote_account_fetcher_worker.take()
        {
            let cancellation_token = self.token.clone();
            self.remote_account_fetcher_handle =
                Some(thread::spawn(move || {
                    create_worker_runtime("remote_account_fetcher_worker")
                        .block_on(async move {
                            remote_account_fetcher_worker
                                .start_fetch_request_processing(
                                    cancellation_token,
                                )
                                .await;
                        });
                }));
        }
    }

    fn start_remote_account_updates_worker(&mut self) {
        if let Some(mut remote_account_updates_worker) =
            self.remote_account_updates_worker.take()
        {
            let cancellation_token = self.token.clone();
            self.remote_account_updates_handle =
                Some(thread::spawn(move || {
                    create_worker_runtime("remote_account_updates_worker")
                        .block_on(async move {
                            remote_account_updates_worker
                                .start_monitoring_request_processing(
                                    cancellation_token,
                                )
                                .await
                        });
                }));
        }
    }

    fn start_remote_account_cloner_worker(&mut self) {
        if let Some(mut remote_account_cloner_worker) =
            self.remote_account_cloner_worker.take()
        {
            let cancellation_token = self.token.clone();
            self.remote_account_cloner_handle =
                Some(thread::spawn(move || {
                    create_worker_runtime("remote_account_cloner_worker")
                        .block_on(async move {
                            remote_account_cloner_worker
                                .start_clone_request_processing(
                                    cancellation_token,
                                )
                                .await
                        });
                }));
        }
    }

    pub fn stop(&self) {
        self.exit.store(true, Ordering::Relaxed);
        self.rpc_service.close();
        PubsubService::close(&self.pubsub_close_handle);
        self.token.cancel();
    }

    pub fn join(&self) {
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

fn create_worker_runtime(thread_name: &str) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .thread_name(thread_name)
        .build()
        .unwrap()
}
