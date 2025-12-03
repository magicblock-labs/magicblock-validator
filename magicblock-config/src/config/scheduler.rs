use serde::{Deserialize, Serialize};

use crate::consts;

/// Configuration for the internal task scheduler.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct TaskSchedulerConfig {
    /// If true, clears all pending scheduled tasks on startup.
    pub reset: bool,
    /// The minimum frequency for a task to be executed, in milliseconds.
    pub min_frequency_millis: i64,
}

impl Default for TaskSchedulerConfig {
    fn default() -> Self {
        Self {
            reset: false,
            min_frequency_millis:
                consts::DEFAULT_TASK_SCHEDULER_MIN_FREQUENCY_MILLIS,
        }
    }
}
