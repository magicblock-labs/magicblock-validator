use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn get_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
}
