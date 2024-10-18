use std::time::{Duration, SystemTime, UNIX_EPOCH};

use conjunto_transwise::RpcCluster;
use sleipnir_mutator::Cluster;
use solana_sdk::genesis_config::ClusterType;
use url::Url;

use crate::errors::{AccountsError, AccountsResult};

pub(crate) fn get_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
}

pub fn try_rpc_cluster_from_cluster(
    cluster: &Cluster,
) -> AccountsResult<RpcCluster> {
    match cluster {
        Cluster::Known(cluster) => {
            use ClusterType::*;
            Ok(match cluster {
                Testnet => RpcCluster::Testnet,
                MainnetBeta => RpcCluster::Mainnet,
                Devnet => RpcCluster::Devnet,
                Development => RpcCluster::Development,
            })
        }
        Cluster::Custom(url) => {
            let ws_url = try_ws_url_from_rpc_url(url.as_str())?;
            Ok(RpcCluster::Custom(url.to_string(), ws_url))
        }
    }
}

fn try_ws_url_from_rpc_url(url: &str) -> AccountsResult<String> {
    // Change http to ws scheme or https to wss
    let mut url = Url::parse(url).map_err(Box::new)?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(AccountsError::InvalidRpcUrl(url.to_string())),
    };
    // Add one to the port if the rpc url has one
    let port = url.port().map(|port| port + 1);

    url.set_scheme(scheme)
        .map_err(|_| AccountsError::FailedToUpdateUrlScheme)?;
    url.set_port(port)
        .map_err(|_| AccountsError::FailedToUpdateUrlPort)?;

    Ok(url.to_string().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert_and_assert(cluster: Cluster, expected_rpc_cluster: RpcCluster) {
        let rpc_cluster = try_rpc_cluster_from_cluster(&cluster).unwrap();
        assert_eq!(rpc_cluster, expected_rpc_cluster);
    }

    #[test]
    fn test_rpc_cluster_from_cluster() {
        convert_and_assert(
            Cluster::Known(ClusterType::Testnet),
            RpcCluster::Testnet,
        );
        convert_and_assert(
            Cluster::Known(ClusterType::MainnetBeta),
            RpcCluster::Mainnet,
        );
        convert_and_assert(
            Cluster::Known(ClusterType::Devnet),
            RpcCluster::Devnet,
        );
        convert_and_assert(
            Cluster::Known(ClusterType::Development),
            RpcCluster::Development,
        );
        convert_and_assert(
            Cluster::Custom("http://localhost:8899".to_string()),
            RpcCluster::Custom(
                "http://localhost:8899".to_string(),
                "ws://localhost:8900".to_string(),
            ),
        );
        convert_and_assert(
            Cluster::Custom("https://some-url.org".to_string()),
            RpcCluster::Custom(
                "https://some-url.org".to_string(),
                "wss://some-url.org".to_string(),
            ),
        );
    }
}
