#![cfg(any(test, feature = "dev-context"))]
#![allow(dead_code)]
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{signature::Keypair, signer::Signer};

use crate::{
    accounts_bank::mock::AccountsBankStub,
    remote_account_provider::{RemoteAccount, RemoteAccountUpdateSource},
};

pub const PUBSUB_URL: &str = "ws://localhost:7800";
pub const RPC_URL: &str = "http://localhost:7799";

#[macro_export]
macro_rules! skip_if_no_test_validator {
    () => {
        if ::std::env::var("LOCAL_VALIDATOR_TESTS").is_err() {
            eprintln!("skipping test, LOCAL_VALIDATOR_TESTS is not set");
            return;
        }
    };
}

pub fn random_pubkey() -> Pubkey {
    Keypair::new().pubkey()
}

pub fn random_pubkeys(n: usize) -> Vec<Pubkey> {
    (0..n).map(|_| random_pubkey()).collect()
}

pub async fn airdrop(rpc_client: &RpcClient, pubkey: &Pubkey, lamports: u64) {
    let sig = rpc_client.request_airdrop(pubkey, lamports).await.unwrap();
    rpc_client.confirm_transaction(&sig).await.unwrap();
}

pub async fn await_next_slot(rpc_client: &RpcClient) {
    let current_slot = rpc_client.get_slot().await.unwrap();

    while rpc_client.get_slot().await.unwrap() == current_slot {
        tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
    }
}

pub async fn current_slot(rpc_client: &RpcClient) -> u64 {
    rpc_client.get_slot().await.unwrap()
}

pub async fn sleep_ms(millis: u64) {
    tokio::time::sleep(tokio::time::Duration::from_millis(millis)).await;
}

pub fn remote_account_lamports(acc: &RemoteAccount) -> u64 {
    acc.account(&AccountsBankStub::default())
        .map(|a| a.lamports())
        .unwrap_or(0)
}

pub fn init_logger() {
    let _ = env_logger::builder()
        .format_timestamp(None)
        .format_module_path(false)
        .format_target(false)
        .format_source_path(true)
        .is_test(true)
        .try_init();
}

pub fn get_remote_account_lamports<'a>(
    all_pubkeys: &'a [Pubkey],
    remote_accounts: &[RemoteAccount],
) -> Vec<(&'a Pubkey, u64)> {
    all_pubkeys
        .iter()
        .zip(remote_accounts)
        .map(|(pk, acc)| {
            let lamports = remote_account_lamports(acc);
            (pk, lamports)
        })
        .collect::<Vec<_>>()
}

pub fn dump_remote_account_lamports(accs: &[(&Pubkey, u64)]) {
    for (pk, lamports) in accs.iter() {
        log::info!("{pk}: {lamports}");
    }
}

pub fn get_remote_account_update_sources<'a>(
    all_pubkeys: &'a [Pubkey],
    remote_accounts: &[RemoteAccount],
) -> Vec<(&'a Pubkey, Option<RemoteAccountUpdateSource>)> {
    all_pubkeys
        .iter()
        .zip(remote_accounts)
        .map(|(pk, acc)| (pk, acc.source()))
        .collect::<Vec<_>>()
}

pub fn dump_remote_account_update_source(
    accs: &[(&Pubkey, Option<RemoteAccountUpdateSource>)],
) {
    for (pk, source) in accs.iter() {
        log::info!("{pk}: {source:?}");
    }
}
