use std::sync::RwLock;

use lazy_static::lazy_static;
use magicblock_config::consts;

lazy_static! {
    static ref MIN_TASK_SCHEDULER_INTERVAL: RwLock<i64> =
        RwLock::new(consts::DEFAULT_TASK_SCHEDULER_MIN_INTERVAL_MILLIS);
}

pub fn min_task_scheduler_interval() -> i64 {
    *MIN_TASK_SCHEDULER_INTERVAL
        .read()
        .expect("RwLock MIN_TASK_SCHEDULER_INTERVAL poisoned")
}

pub fn set_min_task_scheduler_interval(interval: i64) {
    let mut min_task_scheduler_interval_lock = MIN_TASK_SCHEDULER_INTERVAL
        .write()
        .expect("RwLock MIN_TASK_SCHEDULER_INTERVAL poisoned");
    *min_task_scheduler_interval_lock = interval.max(1);
}
