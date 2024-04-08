use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    process,
    sync::Arc,
    time::Duration,
};

use crossbeam_channel::unbounded;
use log::*;
use sleipnir_bank::{bank::Bank, genesis_utils::create_genesis_config};
use sleipnir_rpc::{
    json_rpc_request_processor::JsonRpcConfig, json_rpc_service::JsonRpcService,
};
use sleipnir_rpc_pubsub::pubsub_service::{RpcPubsubConfig, RpcPubsubService};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{signature::Keypair, signer::Signer};
use test_tools::{
    account::{fund_account, fund_account_addr},
    bank::bank_for_tests_with_paths,
    init_logger,
};

use crate::geyser::{init_geyser_service, GeyserTransactionNotifyListener};
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";
mod geyser;

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

    let genesis_config = create_genesis_config(u64::MAX).genesis_config;
    let (geyser_service, geyser_rpc_service) = init_geyser_service()
        .await
        .expect("Failed to init geyser service");

    let transaction_notifier = geyser_service
        .get_transaction_notifier()
        .expect("Failed to get transaction notifier from geyser service");

    let (transaction_sndr, transaction_recvr) = unbounded();
    let transaction_listener = GeyserTransactionNotifyListener::new(
        transaction_notifier,
        transaction_recvr,
    );
    transaction_listener.run();

    let bank = {
        let bank = bank_for_tests_with_paths(
            &genesis_config,
            geyser_service.get_accounts_update_notifier(),
            vec!["/tmp/sleipnir-rpc-bin"],
        );
        Arc::new(bank)
    };
    fund_luzifer(&bank);
    let faucet_keypair = fund_faucet(&bank);

    let tick_duration = Duration::from_millis(100);
    info!(
        "Adding Slot ticker for {}ms slots",
        tick_duration.as_millis()
    );
    init_slot_ticker(bank.clone(), tick_duration);

    // JSON RPC Service
    let json_rpc_service = {
        let config = JsonRpcConfig {
            slot_duration: tick_duration,
            transaction_status_sender: Some(TransactionStatusSender {
                sender: transaction_sndr,
            }),
            ..Default::default()
        };
        let rpc_socket =
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8899);

        // This service needs to run on its own thread as otherwise it affects
        // other tokio runtimes, i.e. the one of the GeyserPlugin
        let hdl = std::thread::spawn(move || {
            let _json_rpc_service = JsonRpcService::new(
                rpc_socket,
                bank.clone(),
                faucet_keypair,
                config,
            )
            .unwrap();
        });
        info!(
            "Launched JSON RPC service with pid {} at {:?}",
            process::id(),
            rpc_socket
        );
        hdl
    };
    // PubSub Service
    RpcPubsubService::spawn(
        RpcPubsubConfig::default(),
        geyser_rpc_service.clone(),
    );

    json_rpc_service.join().unwrap();
}

fn init_slot_ticker(bank: Arc<Bank>, tick_duration: Duration) {
    let bank = bank.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(tick_duration);
        bank.advance_slot();
    });
}
