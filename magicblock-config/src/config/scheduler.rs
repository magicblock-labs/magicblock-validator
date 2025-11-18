use serde::{Deserialize, Serialize};

/// Configuration for the internal task scheduler.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct TaskSchedulerConfig {
    /// If true, clears all pending scheduled tasks on startup.
    pub reset: bool,
}
