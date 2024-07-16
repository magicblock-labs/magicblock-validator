use std::{
    fs,
    net::SocketAddr,
    path::Path,
    process,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use log::*;
use sleipnir_accounts::AccountsManager;
use sleipnir_bank::{
    bank::Bank, genesis_utils::create_genesis_config_with_leader,
    program_loader::load_programs_into_bank,
    transaction_logs::TransactionLogCollectorFilter,
    transaction_notifier_interface::TransactionNotifierArc,
};
use sleipnir_config::{ProgramConfig, SleipnirConfig};
use sleipnir_geyser_plugin::rpc::GeyserRpcService;
use sleipnir_ledger::Ledger;
use sleipnir_perf_service::SamplePerformanceService;
use sleipnir_program::{
    commit_sender::init_commit_channel, init_validator_authority,
};
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
    genesis_config::GenesisConfig, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::{
    errors::{ApiError, ApiResult},
    exernal_config::try_convert_accounts_config,
    fund_account::{fund_validator_identity, funded_faucet},
    geyser_transaction_notify_listener::GeyserTransactionNotifyListener,
    init_geyser_service::{init_geyser_service, InitGeyserServiceConfig},
    tickers::{init_commit_accounts_ticker, init_slot_ticker},
};

// -----------------
// MagicValidatorConfig
// -----------------
#[derive(Default)]
pub struct MagicValidatorConfig {
    pub validator_config: SleipnirConfig,
    pub ledger: Option<Ledger>,
    pub init_geyser_service_config: InitGeyserServiceConfig,
}

impl MagicValidatorConfig {
    pub fn try_from_config_path(config_path: &str) -> ApiResult<Self> {
        Ok(SleipnirConfig::try_load_from_file(config_path).map(
            |validator_config| Self {
                validator_config,
                ..Default::default()
            },
        )?)
    }
    pub fn try_from_config_toml(
        config_toml: &str,
        config_path: Option<&Path>,
    ) -> ApiResult<Self> {
        Ok(
            SleipnirConfig::try_load_from_toml(config_toml, config_path).map(
                |validator_config| Self {
                    validator_config,
                    ..Default::default()
                },
            )?,
        )
    }
}

impl std::fmt::Debug for MagicValidatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MagicValidatorConfig")
            .field("validator_config", &self.validator_config)
            .field(
                "ledger",
                &self
                    .ledger
                    .as_ref()
                    .map(|l| l.ledger_path().display().to_string())
                    .unwrap_or("Not Provided".to_string()),
            )
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
    slot_ticker: Option<std::thread::JoinHandle<()>>,
    pubsub_handle: RwLock<Option<std::thread::JoinHandle<()>>>,
    pubsub_close_handle: PubsubServiceCloseHandle,
    sample_performance_service: Option<SamplePerformanceService>,
    commit_accounts_ticker: Option<tokio::task::JoinHandle<()>>,
    accounts_manager: Arc<AccountsManager>,
    transaction_listener: GeyserTransactionNotifyListener,
    rpc_service: JsonRpcService,
    geyser_rpc_service: Arc<GeyserRpcService>,
    pubsub_config: PubsubConfig,
}

impl MagicValidator {
    // -----------------
    // Initialization
    // -----------------
    pub fn try_from_config(
        config: MagicValidatorConfig,
        identity_keypair: Keypair,
    ) -> ApiResult<Self> {
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
            config.ledger,
            config.validator_config.validator.reset_ledger,
        )?;

        fund_validator_identity(&bank, &validator_pubkey);
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
        let transaction_status_sender = TransactionStatusSender {
            sender: transaction_sndr,
        };
        let accounts_manager = Self::init_accounts_manager(
            &bank,
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
            transaction_status_sender,
            &pubsub_config,
            &config.validator_config,
        )?;

        init_validator_authority(identity_keypair);

        Ok(Self {
            config: config.validator_config,
            exit,
            rpc_service,
            geyser_rpc_service,
            slot_ticker: None,
            commit_accounts_ticker: None,
            pubsub_handle: Default::default(),
            pubsub_close_handle: Default::default(),
            sample_performance_service: None,
            pubsub_config,
            // TODO(thlorenz): @@ this will need to be recreated on each start
            token: CancellationToken::new(),
            bank,
            ledger,
            accounts_manager,
            transaction_listener,
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

        let accounts_manager = Arc::new(accounts_manager);
        if config.accounts.commit.trigger {
            let receiver = init_commit_channel(10);
            AccountsManager::install_manual_commit_trigger(
                &accounts_manager,
                receiver,
            );
        }

        accounts_manager
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
        ledger: Option<Ledger>,
        reset: bool,
    ) -> ApiResult<Arc<Ledger>> {
        let ledger = match ledger {
            Some(ledger) => Arc::new(ledger),
            None => {
                let ledger_path = TempDir::new().unwrap();
                Arc::new(
                    Ledger::open(ledger_path.path())
                        .expect("Expected to be able to open database ledger"),
                )
            }
        };
        if reset {
            let ledger_path = ledger.ledger_path();
            remove_directory_contents_if_exists(ledger_path).map_err(
                |err| {
                    error!(
                        "Error: Unable to remove {}: {}",
                        ledger_path.display(),
                        err
                    );
                    ApiError::UnableToCleanLedgerDirectory(
                        ledger_path.display().to_string(),
                    )
                },
            )?;
        }
        Ok(ledger)
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
            self.ledger.clone(),
            Duration::from_millis(self.config.validator.millis_per_slot),
            self.exit.clone(),
        ));

        self.commit_accounts_ticker = Some(init_commit_accounts_ticker(
            &self.accounts_manager,
            Duration::from_millis(self.config.accounts.commit.frequency_millis),
            self.token.clone(),
        ));

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

fn remove_directory_contents_if_exists(
    dir: &Path,
) -> Result<(), std::io::Error> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.metadata()?.is_dir() {
            fs::remove_dir_all(entry.path())?
        } else {
            fs::remove_file(entry.path())?
        }
    }
    Ok(())
}
