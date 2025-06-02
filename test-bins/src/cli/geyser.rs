use std::net::IpAddr;

use clap::Parser;
use magicblock_api::EphemeralConfig;
use magicblock_config::GeyserGrpcConfig;

#[derive(Parser, Debug, Clone, Copy)]
pub(crate) struct GeyserGrpcArgs {
    /// The address to listen on
    #[arg(long = "geyser-grpc-addr", env = "GEYSER_GRPC_ADDR")]
    grpc_addr: Option<IpAddr>,
    /// The port to listen on
    #[arg(long = "geyser-grpc-port", env = "GEYSER_GRPC_PORT")]
    grpc_port: Option<u16>,
}

impl GeyserGrpcArgs {
    pub fn merge_with_config(&self, config: &mut EphemeralConfig) {
        config.geyser_grpc = GeyserGrpcConfig {
            addr: self.grpc_addr.unwrap_or(config.geyser_grpc.addr),
            port: self.grpc_port.unwrap_or(config.geyser_grpc.port),
        }
    }
}
