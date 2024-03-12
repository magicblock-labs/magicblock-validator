use solana_sdk::genesis_config::ClusterType;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Extension of [solana_sdk::genesis_config::ClusterType] in order to support
/// custom clusters
pub enum Cluster {
    Known(ClusterType),
    Custom(String),
}

impl From<ClusterType> for Cluster {
    fn from(cluster: ClusterType) -> Self {
        Self::Known(cluster)
    }
}
