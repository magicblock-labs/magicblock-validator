use clap::Args;
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};

#[clap_prefix("task-scheduler")]
#[clap_from_serde]
#[derive(
    Debug,
    Default,
    Clone,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Args,
    Mergeable,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TaskSchedulerConfig {
    /// If true, the task scheduler will reset the database on startup.
    #[derive_env_var]
    #[serde(default)]
    pub reset: bool,
}
