use std::env;

use env_logger::Target;
use solana_sdk::signature::Keypair;

pub mod instructions;
pub mod transactions;
pub const TEST_TABLE_CLOSE: bool = cfg!(feature = "test_table_close");

pub async fn sleep_millis(millis: u64) {
    tokio::time::sleep(tokio::time::Duration::from_millis(millis)).await;
}

pub fn init_logger() {
    let mut builder = env_logger::builder();
    builder
        .format_timestamp(None)
        .format_module_path(false)
        .format_target(false)
        .format_source_path(true)
        .is_test(true);

    if let Ok(path) = env::var("TEST_LOG_FILE") {
        builder.target(Target::Pipe(Box::new(
            std::fs::File::create(path).unwrap(),
        )));
    }
    let _ = builder.try_init();
}

pub fn init_logger_target() {
    let _ = env_logger::builder()
        .format_timestamp(None)
        .is_test(true)
        .try_init();
}

/// This is the test authority used in the delegation program
/// https://github.com/magicblock-labs/delegation-program/blob/7fc0ae9a59e48bea5b046b173ea0e34fd433c3c7/tests/fixtures/accounts.rs#L46
/// It is compiled in as the authority for the validator vault when we build via
/// `cargo build-sbf --features=unit_test_config`
pub fn get_validator_auth() -> Keypair {
    const VALIDATOR_AUTHORITY: [u8; 64] = [
        251, 62, 129, 184, 107, 49, 62, 184, 1, 147, 178, 128, 185, 157, 247,
        92, 56, 158, 145, 53, 51, 226, 202, 96, 178, 248, 195, 133, 133, 237,
        237, 146, 13, 32, 77, 204, 244, 56, 166, 172, 66, 113, 150, 218, 112,
        42, 110, 181, 98, 158, 222, 194, 130, 93, 175, 100, 190, 106, 9, 69,
        156, 80, 96, 72,
    ];
    Keypair::from_bytes(&VALIDATOR_AUTHORITY).unwrap()
}
