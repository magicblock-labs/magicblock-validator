use std::collections::HashSet;

use magicblock_accounts::{AccountsConfig, RemoteCluster};
use magicblock_chainlink::remote_account_provider::chain_laser_client::is_helius_laser_url;
use magicblock_config::{errors::ConfigResult, RemoteConfig};
use solana_sdk::pubkey::Pubkey;

const TESTNET_URL: &str = "https://api.testnet.solana.com";
const MAINNET_URL: &str = "https://api.mainnet-beta.solana.com";
const DEVNET_URL: &str = "https://api.devnet.solana.com";
const DEVELOPMENT_URL: &str = "http://127.0.0.1:8899";

const WS_MAINNET: &str = "wss://api.mainnet-beta.solana.com/";
const WS_TESTNET: &str = "wss://api.testnet.solana.com/";
const WS_DEVNET: &str = "wss://api.devnet.solana.com/";
const WS_DEVELOPMENT: &str = "ws://localhost:8900";

pub(crate) fn try_convert_accounts_config(
    conf: &magicblock_config::AccountsConfig,
) -> ConfigResult<AccountsConfig> {
    Ok(AccountsConfig {
        remote_cluster: remote_cluster_from_remote(&conf.remote),
        lifecycle: lifecycle_mode_from_lifecycle_mode(&conf.lifecycle),
        commit_compute_unit_price: conf.commit.compute_unit_price,
        allowed_program_ids: allowed_program_ids_from_allowed_programs(
            &conf.allowed_programs,
        ),
    })
}
pub fn remote_cluster_from_remote(
    remote_config: &RemoteConfig,
) -> RemoteCluster {
    const WS_MULTIPLEX_COUNT: usize = 3;
    use magicblock_config::RemoteCluster::*;
    let (url, ws_url) = match remote_config.cluster {
        Devnet => (
            DEVNET_URL.to_string(),
            vec![WS_DEVNET.to_string(); WS_MULTIPLEX_COUNT],
        ),
        Mainnet => (
            MAINNET_URL.to_string(),
            vec![WS_MAINNET.to_string(); WS_MULTIPLEX_COUNT],
        ),
        Testnet => (
            TESTNET_URL.to_string(),
            vec![WS_TESTNET.to_string(); WS_MULTIPLEX_COUNT],
        ),
        Development => (
            DEVELOPMENT_URL.to_string(),
            vec![WS_DEVELOPMENT.to_string(); 2],
        ),
        Custom => {
            let rpc_url = remote_config
                .url
                .as_ref()
                .expect("rpc url must be set for Custom cluster");
            let ws_urls = remote_config
                .ws_url
                .as_ref()
                .map(|ws_urls| ws_urls.iter().map(|x| x.to_string()).collect())
                .unwrap_or_else(|| {
                    let mut ws_url = rpc_url.clone();
                    // We only multiplex if the ws urls are actually websockets and only
                    // then do we adjust the protocol.
                    // We do not need to do this if we subscribe via GRPC, i.e. helius
                    // laser which is more stable.
                    let is_grpc = is_grpc_url(ws_url.as_ref());
                    if !is_grpc {
                        ws_url
                            .set_scheme(if rpc_url.scheme() == "https" {
                                "wss"
                            } else {
                                "ws"
                            })
                            .expect("valid scheme");
                        if let Some(port) = ws_url.port() {
                            ws_url
                                .set_port(Some(port + 1))
                                .expect("valid url with port");
                        }
                    }
                    vec![
                        ws_url.to_string();
                        if is_grpc { 1 } else { WS_MULTIPLEX_COUNT }
                    ]
                });
            (rpc_url.to_string(), ws_urls)
        }
        CustomWithWs => {
            let rpc_url = remote_config
                .url
                .as_ref()
                .expect("rpc url must be set for CustomWithMultipleWs")
                .to_string();
            let ws_url = remote_config
                .ws_url
                .as_ref()
                .expect("ws urls must be set for CustomWithMultipleWs")
                .first()
                .expect("at least one ws url must be set for CustomWithWs")
                .to_string();
            let is_grpc = is_grpc_url(&ws_url.to_string());
            let ws_urls =
                vec![ws_url; if is_grpc { 1 } else { WS_MULTIPLEX_COUNT }];
            (rpc_url, ws_urls)
        }
        CustomWithMultipleWs => {
            // NOTE: we assume that if multiple ws urls are provided the user wants
            //       to multiplex no matter if any is a GRPC based pubsub.
            let rpc_url = remote_config
                .url
                .as_ref()
                .expect("rpc url must be set for CustomWithMultipleWs")
                .to_string();
            let ws_urls = remote_config
                .ws_url
                .as_ref()
                .expect("ws urls must be set for CustomWithMultipleWs")
                .iter()
                .map(|x| x.to_string())
                .collect();
            (rpc_url, ws_urls)
        }
    };
    RemoteCluster {
        url,
        ws_urls: ws_url,
    }
}

fn lifecycle_mode_from_lifecycle_mode(
    clone: &magicblock_config::LifecycleMode,
) -> magicblock_accounts::LifecycleMode {
    use magicblock_config::LifecycleMode::*;
    match clone {
        ProgramsReplica => magicblock_accounts::LifecycleMode::ProgramsReplica,
        Replica => magicblock_accounts::LifecycleMode::Replica,
        Ephemeral => magicblock_accounts::LifecycleMode::Ephemeral,
        Offline => magicblock_accounts::LifecycleMode::Offline,
    }
}

fn allowed_program_ids_from_allowed_programs(
    allowed_programs: &[magicblock_config::AllowedProgram],
) -> Option<HashSet<Pubkey>> {
    if !allowed_programs.is_empty() {
        Some(HashSet::from_iter(
            allowed_programs
                .iter()
                .map(|allowed_program| allowed_program.id),
        ))
    } else {
        None
    }
}

fn is_grpc_url(url: &str) -> bool {
    is_helius_laser_url(url)
}
