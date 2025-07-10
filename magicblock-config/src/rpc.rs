use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use clap::Args;
use magicblock_config_macro::{clap_from_serde, clap_prefix};
use serde::{Deserialize, Serialize};

#[clap_prefix("rpc")]
#[clap_from_serde]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct RpcConfig {
    #[derive_env_var]
    #[arg(help = "The address the RPC will listen on.")]
    #[serde(
        default = "default_addr",
        deserialize_with = "deserialize_addr",
        serialize_with = "serialize_addr"
    )]
    pub addr: IpAddr,
    #[derive_env_var]
    #[arg(help = "The port the RPC will listen on.")]
    #[serde(default = "default_port")]
    pub port: u16,
    #[arg(help = "The max number of WebSocket connections to accept.")]
    #[serde(default = "default_max_ws_connections")]
    pub max_ws_connections: usize,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            addr: default_addr(),
            port: default_port(),
            max_ws_connections: default_max_ws_connections(),
        }
    }
}

impl RpcConfig {
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.addr, self.port)
    }
}

fn clap_deserialize_addr(s: &str) -> Result<IpAddr, String> {
    s.parse().map_err(|err| format!("Invalid address: {err}"))
}

fn deserialize_addr<'de, D>(deserializer: D) -> Result<IpAddr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.parse().map_err(|err| {
        // The error returned here by serde is a bit unhelpful so we help out
        // by logging a bit more information.
        eprintln!("The [rpc] field 'addr' is invalid ({err:?}).");
        serde::de::Error::custom(err)
    })
}

fn serialize_addr<S>(addr: &IpAddr, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(addr.to_string().as_ref())
}

fn default_addr() -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))
}

fn default_port() -> u16 {
    8899
}

fn default_max_ws_connections() -> usize {
    16384
}
