use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    process,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use crossbeam_channel::unbounded;
use log::*;
use sleipnir_accounts::AccountsManager;
use sleipnir_bank::{
    bank::Bank,
    genesis_utils::{create_genesis_config, GenesisConfigInfo},
};
use sleipnir_config::{ProgramConfig, SleipnirConfig};
use sleipnir_ledger::Ledger;
use sleipnir_perf_service::SamplePerformanceService;
use sleipnir_program::set_validator_authority;
use sleipnir_pubsub::pubsub_service::{PubsubConfig, PubsubService};
use sleipnir_rpc::{
    json_rpc_request_processor::JsonRpcConfig, json_rpc_service::JsonRpcService,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use tempfile::TempDir;
use test_tools::{
    account::{fund_account, fund_account_addr},
    bank::bank_for_tests_with_paths,
    init_logger,
    programs::{load_programs_from_config, load_programs_from_string_config},
};
use utils::timestamp_in_secs;

use crate::{
    geyser::{init_geyser_service, GeyserTransactionNotifyListener},
    utils::{try_convert_accounts_config, TEST_KEYPAIR_BYTES},
};
mod geyser;
mod utils;

fn fund_validator_identity(bank: &Bank, validator_id: &Pubkey) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account(bank, validator_id, u64::MAX / 2);
}

fn fund_faucet(bank: &Bank) -> Keypair {
    let faucet = Keypair::new();
    fund_account(bank, &faucet.pubkey(), u64::MAX / 2);
    faucet
}

#[tokio::main]
async fn main() {
    init_logger!();

    #[cfg(feature = "tokio-console")]
    console_subscriber::init();
    let (file, config) = load_config_from_arg();
    match file {
        Some(file) => info!("Loading config from '{}'.", file),
        None => info!("Using default config. Override it by passing the path to a config file."),
    };
    info!("Starting validator with config:\n{}", config);

    let exit = Arc::<AtomicBool>::default();

    let validator_keypair = validator_keypair();

    let GenesisConfigInfo {
        genesis_config,
        validator_pubkey,
        ..
    } = create_genesis_config(u64::MAX, &validator_keypair.pubkey());
    let (geyser_service, geyser_rpc_service) = init_geyser_service()
        .await
        .expect("Failed to init geyser service");

    let transaction_notifier = geyser_service.get_transaction_notifier();

    let ledger_path = TempDir::new().unwrap();
    let ledger = Arc::new(
        Ledger::open(ledger_path.path())
            .expect("Expected to be able to open database ledger"),
    );

    let (transaction_sndr, transaction_recvr) = unbounded();
    let transaction_listener = GeyserTransactionNotifyListener::new(
        transaction_notifier,
        transaction_recvr,
        ledger.clone(),
    );
    transaction_listener.run(true);

    let bank = {
        let bank = bank_for_tests_with_paths(
            &genesis_config,
            geyser_service.get_accounts_update_notifier(),
            geyser_service.get_slot_status_notifier(),
            validator_pubkey,
            vec!["/tmp/sleipnir-rpc-bin"],
        );
        Arc::new(bank)
    };
    fund_validator_identity(&bank, &validator_pubkey);
    load_programs(&bank, &config.programs).unwrap();

    SamplePerformanceService::new(&bank, &ledger, exit);
    let faucet_keypair = fund_faucet(&bank);

    let tick_millis = config.validator.millis_per_slot;
    let tick_duration = Duration::from_millis(tick_millis);

    let pubsub_config = PubsubConfig::from_rpc(config.rpc.port);
    // JSON RPC Service
    let json_rpc_service = {
        let transaction_status_sender = TransactionStatusSender {
            sender: transaction_sndr,
        };
        let rpc_socket_addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            config.rpc.port,
        );
        let rpc_json_config = JsonRpcConfig {
            slot_duration: tick_duration,
            genesis_creation_time: genesis_config.creation_time,
            transaction_status_sender: Some(transaction_status_sender.clone()),
            rpc_socket_addr: Some(rpc_socket_addr),
            pubsub_socket_addr: Some(*pubsub_config.socket()),
            enable_rpc_transaction_history: true,

            ..Default::default()
        };

        let accounts_manager = {
            let accounts_config =  try_convert_accounts_config(&config.accounts).expect("Failed to derive accounts config from provided sleipnir config");
            let accounts_manager = AccountsManager::try_new(
                &bank,
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
        };

        set_validator_authority(validator_keypair);

        // -----------------
        // Tickers
        // -----------------
        info!(
            "Adding Slot ticker for {}ms slots",
            tick_duration.as_millis()
        );
        init_slot_ticker(bank.clone(), ledger.clone(), tick_duration);
        init_commit_accounts_ticker(
            &accounts_manager,
            Duration::from_millis(config.accounts.commit.frequency_millis),
        );

        // This service needs to run on its own thread as otherwise it affects
        // other tokio runtimes, i.e. the one of the GeyserPlugin
        let hdl = {
            let bank = bank.clone();
            std::thread::spawn(move || {
                let _json_rpc_service = JsonRpcService::new(
                    bank,
                    ledger.clone(),
                    faucet_keypair,
                    genesis_config.hash(),
                    accounts_manager,
                    rpc_json_config,
                )
                .unwrap();
            })
        };
        info!(
            "Launched JSON RPC service with pid {} at {:?}",
            process::id(),
            rpc_socket_addr
        );
        hdl
    };
    // PubSub Service
    let pubsub_service = PubsubService::spawn(
        pubsub_config,
        geyser_rpc_service.clone(),
        bank.clone(),
    );

    info!("Validator identity: {}", validator_pubkey);

    json_rpc_service.join().unwrap();
    pubsub_service.join().unwrap();
}

fn validator_keypair() -> Keypair {
    // 1. Try to load it from an env var base58 encoded
    if let Ok(keypair) = std::env::var("VALIDATOR_KEYPAIR") {
        Keypair::from_base58_string(&keypair)
    } else {
        warn!("Using default test keypair, provide one by setting 'VALIDATOR_KEYPAIR' env var to a base58 encoded private key");
        Keypair::from_bytes(&TEST_KEYPAIR_BYTES)
            // SAFETY: these bytes are compiled into the code, thus we know it is valid
            .unwrap()
    }
}

fn init_slot_ticker(
    bank: Arc<Bank>,
    ledger: Arc<Ledger>,
    tick_duration: Duration,
) {
    let bank = bank.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(tick_duration);
        let slot = bank.advance_slot();
        let _ = ledger
            .cache_block_time(slot, timestamp_in_secs() as i64)
            .map_err(|e| {
                error!("Failed to cache block time: {:?}", e);
            });
    });
}

fn init_commit_accounts_ticker(
    manager: &Arc<AccountsManager>,
    tick_duration: Duration,
) {
    let manager = manager.clone();
    tokio::task::spawn(async move {
        loop {
            tokio::time::sleep(tick_duration).await;
            let sigs = manager.commit_delegated().await;
            match sigs {
                Ok(sigs) if sigs.is_empty() => {
                    trace!("No accounts committed");
                }
                Ok(sigs) => {
                    debug!("Commits: {:?}", sigs);
                }
                Err(err) => {
                    error!("Failed to commit accounts: {:?}", err);
                }
            }
        }
    });
}

fn load_programs(
    bank: &Bank,
    programs: &[ProgramConfig],
) -> Result<(), Box<dyn std::error::Error>> {
    // Keep supporting the old way of loading programs, but phase out eventually
    if let Ok(programs) = std::env::var("PROGRAMS") {
        load_programs_from_string_config(bank, &programs)?;
    }

    load_programs_from_config(bank, programs)
}

fn load_config_from_arg() -> (Option<String>, SleipnirConfig) {
    let config_file = std::env::args().nth(1);
    match config_file {
        Some(config_file) => {
            let config = SleipnirConfig::try_load_from_file(&config_file)
                .unwrap_or_else(|err| {
                    panic!(
                        "Failed to load config file from '{}'. ({})",
                        config_file, err
                    )
                });
            (Some(config_file), config)
        }
        None => (None, Default::default()),
    }
}
