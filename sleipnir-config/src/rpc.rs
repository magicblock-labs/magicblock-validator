use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct RpcConfig {
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
        }
    }
}

fn default_port() -> u16 {
    8899
}
