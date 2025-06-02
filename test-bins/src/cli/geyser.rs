use std::net::IpAddr;

use clap::Parser;
use magicblock_config::GeyserGrpcConfig;

#[derive(Parser, Debug, Clone, Copy)]
pub(crate) struct GeyserGrpcArgs {
    /// The address to listen on
    #[arg(
        long = "geyser-grpc-addr",
        default_value = "0.0.0.0",
        env = "GEYSER_GRPC_ADDR"
    )]
    grpc_addr: IpAddr,
    /// The port to listen on
    #[arg(
        long = "geyser-grpc-port",
        default_value = "10000",
        env = "GEYSER_GRPC_PORT"
    )]
    grpc_port: u16,
}

impl From<GeyserGrpcArgs> for GeyserGrpcConfig {
    fn from(value: GeyserGrpcArgs) -> Self {
        GeyserGrpcConfig {
            addr: value.grpc_addr,
            port: value.grpc_port,
        }
    }
}
