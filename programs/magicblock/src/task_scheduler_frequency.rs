use std::sync::RwLock;

use lazy_static::lazy_static;
use magicblock_config::consts;

lazy_static! {
    static ref MIN_TASK_SCHEDULER_FREQUENCY: RwLock<i64> =
        RwLock::new(consts::DEFAULT_TASK_SCHEDULER_MIN_FREQUENCY_MILLIS);
}

pub fn min_task_scheduler_frequency() -> i64 {
    *MIN_TASK_SCHEDULER_FREQUENCY
        .read()
        .expect("RwLock MIN_TASK_SCHEDULER_FREQUENCY poisoned")
}

pub fn set_min_task_scheduler_frequency(frequency: i64) {
    let mut min_task_scheduler_frequency_lock = MIN_TASK_SCHEDULER_FREQUENCY
        .write()
        .expect("RwLock MIN_TASK_SCHEDULER_FREQUENCY poisoned");
    *min_task_scheduler_frequency_lock = frequency.max(1);
}
