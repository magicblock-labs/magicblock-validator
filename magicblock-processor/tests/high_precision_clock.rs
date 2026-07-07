use std::time::Duration;

use magicblock_magic_program_api::sysvar::{
    HighPrecisionClock, HIGH_PRECISION_CLOCK_ID,
};
use solana_account::ReadableAccount;
use solana_program::clock::Clock;
use solana_sdk_ids::sysvar::clock;
use test_kit::ExecutionTestEnv;

fn read_clock(env: &ExecutionTestEnv) -> Clock {
    let account = env.get_account(clock::ID);
    bincode::deserialize(account.data())
        .expect("failed to deserialize Clock sysvar")
}

fn read_high_precision_clock(env: &ExecutionTestEnv) -> HighPrecisionClock {
    let account = env.get_account(HIGH_PRECISION_CLOCK_ID);
    bincode::deserialize(account.data())
        .expect("failed to deserialize HighPrecisionClock sysvar")
}

#[tokio::test]
async fn test_high_precision_clock_tracks_clock() {
    let env = ExecutionTestEnv::new();

    // Let the primary scheduler produce a few blocks so the sysvar is updated
    // from real wall-clock time rather than only the genesis value.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut hpc = read_high_precision_clock(&env);
    // A real timestamp must have been sampled from wall-clock time.
    assert!(
        hpc.unix_timestamp_millis > 0,
        "unix_timestamp_millis not set: {}",
        hpc.unix_timestamp_millis
    );

    // Read a consistent (same-block) snapshot of both sysvars. A slot boundary
    // may land between the two reads, so retry until they agree. The
    // millisecond timestamp divided down to whole seconds must equal Clock.
    let mut consistent = false;
    for _ in 0..50 {
        let clock = read_clock(&env);
        hpc = read_high_precision_clock(&env);
        if hpc.unix_timestamp_millis.div_euclid(1000) == clock.unix_timestamp {
            consistent = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        consistent,
        "HighPrecisionClock timestamp never matched Clock.unix_timestamp"
    );
}
