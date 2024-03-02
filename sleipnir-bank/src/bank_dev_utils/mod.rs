mod bank;
pub mod elfs;
pub mod transactions;
pub use bank::*;

pub fn init_logger() {
    let _ = env_logger::builder()
        .format_timestamp_micros()
        .is_test(true)
        .try_init();
}
