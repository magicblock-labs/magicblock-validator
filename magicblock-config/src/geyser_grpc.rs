use crate::helpers;

helpers::socket_addr_config! {
    GeyserGrpcConfig,
    10_000,
    "geyser_grpc",
    "geyser_grpc"
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use magicblock_config_helpers::Merge;

    use super::*;

    #[test]
    fn test_merge_with_default() {
        let mut config = GeyserGrpcConfig {
            addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
            port: 9090,
        };
        let original_config = config.clone();
        let other = GeyserGrpcConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_merge_default_with_non_default() {
        let mut config = GeyserGrpcConfig::default();
        let other = GeyserGrpcConfig {
            addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
            port: 9090,
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_merge_non_default() {
        let mut config = GeyserGrpcConfig {
            addr: IpAddr::V4(Ipv4Addr::new(0, 0, 1, 127)),
            port: 9091,
        };
        let original_config = config.clone();
        let other = GeyserGrpcConfig {
            addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
            port: 9090,
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }
}
