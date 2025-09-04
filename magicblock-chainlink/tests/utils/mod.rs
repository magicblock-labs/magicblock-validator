#![cfg(any(test, feature = "dev-context"))]

pub mod accounts;
pub mod logging;
pub mod test_context;

#[allow(dead_code)]
pub async fn sleep_ms(ms: u64) {
    use std::time::Duration;
    tokio::time::sleep(Duration::from_millis(ms)).await;
}
