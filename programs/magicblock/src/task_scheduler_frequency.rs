use std::sync::atomic::{AtomicI64, Ordering};

use lazy_static::lazy_static;
use magicblock_config::consts;

lazy_static! {
    static ref MIN_TASK_SCHEDULER_INTERVAL: AtomicI64 =
        AtomicI64::new(consts::DEFAULT_TASK_SCHEDULER_MIN_INTERVAL_MILLIS);
}

pub fn min_task_scheduler_interval() -> i64 {
    MIN_TASK_SCHEDULER_INTERVAL.load(Ordering::Relaxed)
}

pub fn set_min_task_scheduler_interval(interval: i64) {
    MIN_TASK_SCHEDULER_INTERVAL.store(interval.max(1), Ordering::Relaxed);
}
