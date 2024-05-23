use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    process,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use crossbeam_channel::unbounded;
use log::*;
use sleipnir_accounts::{AccountsManager, Cluster};
use sleipnir_bank::{
    bank::Bank,
    genesis_utils::{create_genesis_config, GenesisConfigInfo},
};
use sleipnir_config::{ProgramConfig, SleipnirConfig};
use sleipnir_ledger::Ledger;
use sleipnir_perf_service::SamplePerformanceService;
use sleipnir_pubsub::pubsub_service::{PubsubConfig, PubsubService};
use sleipnir_rpc::{
    json_rpc_request_processor::JsonRpcConfig, json_rpc_service::JsonRpcService,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{
    genesis_config::ClusterType, signature::Keypair, signer::Signer,
};
use tempfile::TempDir;
use test_tools::{
    account::{fund_account, fund_account_addr},
    bank::bank_for_tests_with_paths,
    init_logger,
    programs::{load_programs_from_config, load_programs_from_string_config},
};
use utils::timestamp_in_secs;

use crate::geyser::{init_geyser_service, GeyserTransactionNotifyListener};
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";
mod geyser;
mod utils;

fn fund_luzifer(bank: &Bank) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account_addr(bank, LUZIFER, u64::MAX / 2);
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

    let GenesisConfigInfo {
        genesis_config,
        validator_pubkey,
        ..
    } = create_genesis_config(u64::MAX);
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
    fund_luzifer(&bank);
    load_programs(&bank, &config.programs).unwrap();

    SamplePerformanceService::new(&bank, &ledger, exit);
    let faucet_keypair = fund_faucet(&bank);

    let tick_millis = config.validator.millis_per_slot;
    let tick_duration = Duration::from_millis(tick_millis);
    info!(
        "Adding Slot ticker for {}ms slots",
        tick_duration.as_millis()
    );
    init_slot_ticker(bank.clone(), ledger.clone(), tick_duration);

    let pubsub_config = PubsubConfig::default();
    // JSON RPC Service
    let json_rpc_service = {
        let transaction_status_sender = TransactionStatusSender {
            sender: transaction_sndr,
        };
        let rpc_socket_addr =
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8899);
        let config = JsonRpcConfig {
            slot_duration: tick_duration,
            genesis_creation_time: genesis_config.creation_time,
            transaction_status_sender: Some(transaction_status_sender.clone()),
            rpc_socket_addr: Some(rpc_socket_addr),
            pubsub_socket_addr: Some(*pubsub_config.socket()),
            enable_rpc_transaction_history: true,

            ..Default::default()
        };

        // This service needs to run on its own thread as otherwise it affects
        // other tokio runtimes, i.e. the one of the GeyserPlugin
        let hdl = {
            let bank = bank.clone();
            let accounts_manager = AccountsManager::try_new(
                Cluster::Known(ClusterType::Devnet),
                &bank,
                Some(transaction_status_sender),
                Default::default(),
            )
            .expect("Failed to create accounts manager");
            std::thread::spawn(move || {
                let _json_rpc_service = JsonRpcService::new(
                    bank,
                    ledger.clone(),
                    faucet_keypair,
                    genesis_config.hash(),
                    accounts_manager,
                    config,
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

    json_rpc_service.join().unwrap();
    pubsub_service.join().unwrap();
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
