use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::consts;

/// Configuration for the internal task scheduler.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct TaskSchedulerConfig {
    /// If true, clears all pending scheduled tasks on startup.
    pub reset: bool,
    /// The minimum interval between task executions.
    /// Supports humantime (e.g. "10ms", "1s").
    #[serde(with = "humantime")]
    pub min_interval: Duration,
    /// How long failed task and scheduling records are retained.
    /// Supports humantime (e.g. "1h", "7d").
    #[serde(with = "humantime")]
    pub failed_task_retention: Duration,
    /// How often failed task and scheduling records are cleaned up.
    /// Supports humantime (e.g. "1m", "1h").
    #[serde(with = "humantime")]
    pub failed_task_cleanup_interval: Duration,
}

impl Default for TaskSchedulerConfig {
    fn default() -> Self {
        Self {
            reset: false,
            min_interval: Duration::from_millis(
                consts::DEFAULT_TASK_SCHEDULER_MIN_INTERVAL_MILLIS,
            ),
            failed_task_retention: Duration::from_secs(
                consts::DEFAULT_TASK_SCHEDULER_FAILED_TASK_RETENTION_SECS,
            ),
            failed_task_cleanup_interval: Duration::from_secs(
                consts::DEFAULT_TASK_SCHEDULER_FAILED_TASK_CLEANUP_INTERVAL_SECS,
            ),
        }
    }
}
