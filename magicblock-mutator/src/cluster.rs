use solana_rpc_client_api::client_error::reqwest::Url;
use solana_sdk::genesis_config::ClusterType;

pub const TESTNET_URL: &str = "https://api.testnet.solana.com";
pub const MAINNET_URL: &str = "https://api.mainnet-beta.solana.com";
pub const DEVNET_URL: &str = "https://api.devnet.solana.com";
pub const DEVELOPMENT_URL: &str = "http://127.0.0.1:8899";

const WS_MAINNET: &str = "wss://api.mainnet-beta.solana.com/";
const WS_TESTNET: &str = "wss://api.testnet.solana.com/";
pub const WS_DEVNET: &str = "wss://api.devnet.solana.com/";
const WS_DEVELOPMENT: &str = "ws://localhost:8900";

/// TODO(vbrunet)
///  - this probably belong in a different crate, "mutator" is specific to the data dump mechanisms
///  - conjunto_addresses::cluster::RpcCluster already achieve this and is a full duplicate
///  - deprecation tracked here: https://github.com/magicblock-labs/magicblock-validator/issues/138
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cluster {
    Known(ClusterType),
    Custom(Url),
    CustomWithWs(Url, Url),
    CustomWithMultipleWs { http: Url, ws: Vec<Url> },
}

impl From<ClusterType> for Cluster {
    fn from(cluster: ClusterType) -> Self {
        Self::Known(cluster)
    }
}

impl Cluster {
    pub fn url(&self) -> &str {
        use ClusterType::*;
        match self {
            Cluster::Known(cluster) => match cluster {
                Testnet => TESTNET_URL,
                MainnetBeta => MAINNET_URL,
                Devnet => DEVNET_URL,
                Development => DEVELOPMENT_URL,
            },
            Cluster::Custom(url) => url.as_str(),
            Cluster::CustomWithWs(url, _) => url.as_str(),
            Cluster::CustomWithMultipleWs { http, .. } => http.as_str(),
        }
    }

    pub fn ws_urls(&self) -> Vec<String> {
        use ClusterType::*;
        const WS_SHARD_COUNT: usize = 3;
        match self {
            Cluster::Known(cluster) => vec![
                match cluster {
                    Testnet => WS_TESTNET.into(),
                    MainnetBeta => WS_MAINNET.into(),
                    Devnet => WS_DEVNET.into(),
                    Development => WS_DEVELOPMENT.into(),
                };
                WS_SHARD_COUNT
            ],
            Cluster::Custom(url) => {
                let mut ws_url = url.clone();
                ws_url
                    .set_scheme(if url.scheme() == "https" {
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
                vec![ws_url.to_string(); WS_SHARD_COUNT]
            }
            Cluster::CustomWithWs(_, ws) => {
                vec![ws.to_string(); WS_SHARD_COUNT]
            }
            Cluster::CustomWithMultipleWs { ws, .. } => {
                ws.iter().map(Url::to_string).collect()
            }
        }
    }
}
