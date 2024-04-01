use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    process,
    sync::Arc,
    time::Duration,
};

use log::*;
use sleipnir_bank::{bank::Bank, genesis_utils::create_genesis_config};
use sleipnir_rpc::{
    json_rpc_request_processor::JsonRpcConfig, json_rpc_service::JsonRpcService,
};
use solana_sdk::{signature::Keypair, signer::Signer};
use test_tools::{
    account::{fund_account, fund_account_addr},
    bank::bank_for_tests,
    init_logger,
};
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";

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
    let bank = {
        let bank = bank_for_tests(&genesis_config, None);
        Arc::new(bank)
    };
    fund_luzifer(&bank);
    let faucet_keypair = fund_faucet(&bank);

    let rpc_socket =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8899);
    let tick_duration = Duration::from_millis(100);
    info!(
        "Adding Slot ticker for {}ms slots",
        tick_duration.as_millis()
    );
    init_slot_ticker(bank.clone(), tick_duration);

    info!(
        "Launching JSON RPC service with pid {} at {:?}",
        process::id(),
        rpc_socket
    );
    let config = JsonRpcConfig {
        slot_duration: tick_duration,
        ..Default::default()
    };
    let _json_rpc_service =
        JsonRpcService::new(rpc_socket, bank.clone(), faucet_keypair, config)
            .unwrap();
    info!("Launched JSON RPC service at {:?}", rpc_socket);
}

fn init_slot_ticker(bank: Arc<Bank>, tick_duration: Duration) {
    let bank = bank.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(tick_duration);
        bank.advance_slot();
    });
}
