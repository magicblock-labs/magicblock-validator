use std::net::IpAddr;

use clap::Parser;
use magicblock_config::RpcConfig;

#[derive(Parser, Debug, Clone, Copy)]
pub(crate) struct RpcArgs {
    /// The address to listen on
    #[arg(long = "rpc-addr", default_value = "0.0.0.0", env = "RPC_ADDR")]
    addr: IpAddr,
    /// The port to listen on
    #[arg(long = "rpc-port", default_value = "8899", env = "RPC_PORT")]
    port: u16,
}

impl From<RpcArgs> for RpcConfig {
    fn from(value: RpcArgs) -> Self {
        RpcConfig {
            addr: value.addr,
            port: value.port,
        }
    }
}
