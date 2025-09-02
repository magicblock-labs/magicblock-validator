use clap::Args;
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};

#[clap_prefix("task-scheduler")]
#[clap_from_serde]
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Args, Mergeable,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TaskSchedulerConfig {
    /// The path to the task scheduler database file.
    #[derive_env_var]
    #[serde(default = "default_db_path")]
    pub path: String,
    /// If true, the task scheduler will reset the database on startup.
    #[derive_env_var]
    #[serde(default)]
    pub reset: bool,
    /// Determines how frequently the task scheduler will check for executable tasks.
    #[derive_env_var]
    #[serde(default = "default_millis_per_tick")]
    pub millis_per_tick: u64,
}

impl Default for TaskSchedulerConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
            reset: bool::default(),
            millis_per_tick: default_millis_per_tick(),
        }
    }
}

fn default_db_path() -> String {
    "target/task_scheduler.db".to_string()
}

fn default_millis_per_tick() -> u64 {
    50
}
