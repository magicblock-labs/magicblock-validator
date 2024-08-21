use solana_sdk::genesis_config::ClusterType;

pub const TESTNET_URL: &str = "https://api.testnet.solana.com";
pub const MAINNET_URL: &str = "https://api.mainnet-beta.solana.com";
pub const DEVNET_URL: &str = "https://api.devnet.solana.com";
pub const DEVELOPMENT_URL: &str = "http://127.0.0.1:8899";

#[derive(Debug, Clone, PartialEq, Eq)]
/// Extension of [solana_sdk::genesis_config::ClusterType] in order to support
/// custom clusters
// TODO(vbrunet) - this probably belong in a different crate
//                 (mutator is specific to the data dump mechanism)
pub enum Cluster {
    Known(ClusterType),
    Custom(String),
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
            Cluster::Custom(url) => url,
        }
    }
}
