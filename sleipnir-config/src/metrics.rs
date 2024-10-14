use crate::helpers;
use serde::{Deserialize, Serialize};

helpers::socket_addr_config! {
    MetricsServiceConfig,
    9_000,
    "metrics_service"
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct MetricsConfig {
    #[serde(default = "helpers::serde_defaults::bool_true")]
    pub enabled: bool,
    #[serde(default)]
    #[serde(flatten)]
    pub service: MetricsServiceConfig,
}
