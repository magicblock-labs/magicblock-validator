use std::net::IpAddr;

use clap::Parser;
use magicblock_api::EphemeralConfig;
use magicblock_config::RpcConfig;

#[derive(Parser, Debug, Clone, Copy)]
pub(crate) struct RpcArgs {
    /// The address to listen on
    #[arg(long = "rpc-addr", env = "RPC_ADDR")]
    addr: Option<IpAddr>,
    /// The port to listen on
    #[arg(long = "rpc-port", env = "RPC_PORT")]
    port: Option<u16>,
}

impl RpcArgs {
    pub fn merge_with_config(&self, config: &mut EphemeralConfig) {
        config.rpc = RpcConfig {
            addr: self.addr.unwrap_or(config.rpc.addr),
            port: self.port.unwrap_or(config.rpc.port),
        }
    }
}
