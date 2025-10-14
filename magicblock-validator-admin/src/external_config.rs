use magicblock_mutator::Cluster;
use solana_sdk::genesis_config::ClusterType;

pub(crate) fn cluster_from_remote(
    remote: &magicblock_config::RemoteConfig,
) -> Cluster {
    use magicblock_config::RemoteCluster::*;

    match remote.cluster {
        Devnet => Cluster::Known(ClusterType::Devnet),
        Mainnet => Cluster::Known(ClusterType::MainnetBeta),
        Testnet => Cluster::Known(ClusterType::Testnet),
        Development => Cluster::Known(ClusterType::Development),
        Custom => Cluster::Custom(
            remote.url.clone().expect("Custom remote must have a url"),
        ),
        CustomWithWs => Cluster::CustomWithWs(
            remote
                .url
                .clone()
                .expect("CustomWithWs remote must have a url"),
            remote
                .ws_url
                .clone()
                .expect("CustomWithWs remote must have a ws_url")
                .first()
                .expect("CustomWithWs remote must have at least one ws_url")
                .clone(),
        ),
        CustomWithMultipleWs => Cluster::CustomWithMultipleWs {
            http: remote
                .url
                .clone()
                .expect("CustomWithMultipleWs remote must have a url"),
            ws: remote
                .ws_url
                .clone()
                .expect("CustomWithMultipleWs remote must have a ws_url"),
        },
    }
}
