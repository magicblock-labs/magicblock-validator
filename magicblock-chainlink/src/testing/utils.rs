#![cfg(any(test, feature = "dev-context"))]
#![allow(dead_code)]
use std::sync::Arc;

use magicblock_config::config::LifecycleMode;
use solana_account::ReadableAccount;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;

use crate::remote_account_provider::{
    RemoteAccount, RemoteAccountUpdateSource, ResolvedAccount,
    SubscribedAccounts, config::RemoteAccountProviderConfig,
};

pub const PUBSUB_URL: &str = "ws://localhost:7800";
pub const RPC_URL: &str = "http://localhost:7799";

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
    match acc {
        RemoteAccount::Found(state) => match &state.account {
            ResolvedAccount::Fresh(account) => account.lamports(),
            ResolvedAccount::Bank(_) => 0,
        },
        RemoteAccount::NotFound(_) => 0,
    }
}

pub fn init_logger() {
    magicblock_core::logger::init_for_tests();
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
        tracing::info!(pubkey = %pk, lamports = %lamports, "Account");
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
        tracing::info!(pubkey = %pk, source = ?source, "Account update source");
    }
}

pub fn create_test_subscribed_accounts()
-> (Arc<SubscribedAccounts>, RemoteAccountProviderConfig) {
    let config = RemoteAccountProviderConfig::try_new_with_metrics(
        LifecycleMode::Ephemeral,
        false,
    )
    .unwrap();
    (create_test_subscribed_accounts_with_config(&config), config)
}

pub fn create_test_subscribed_accounts_with_config(
    config: &RemoteAccountProviderConfig,
) -> Arc<SubscribedAccounts> {
    let _ = config;
    Arc::new(SubscribedAccounts::default())
}
