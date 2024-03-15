use log::*;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    process,
    sync::Arc,
    time::Duration,
};

use sleipnir_bank::{bank::Bank, genesis_utils::create_genesis_config};
use sleipnir_rpc::json_rpc_service::JsonRpcService;
use test_tools::{
    account::fund_account_addr, bank::bank_for_tests, init_logger,
};
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";

fn fund_luzifer(bank: &Bank) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account_addr(bank, LUZIFER, u64::MAX / 2);
}

#[tokio::main]
async fn main() {
    init_logger!();

    let genesis_config = create_genesis_config(u64::MAX).genesis_config;
    let bank = {
        let bank = bank_for_tests(&genesis_config);
        Arc::new(bank)
    };
    fund_luzifer(&bank);
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
    let _json_rpc_service =
        JsonRpcService::new(rpc_socket, bank.clone()).unwrap();
    info!("Launched JSON RPC service at {:?}", rpc_socket);
}

fn init_slot_ticker(bank: Arc<Bank>, tick_duration: Duration) {
    let bank = bank.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(tick_duration);
        bank.advance_slot();
    });
}
