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
    // Sub-second remainder must always be a valid fraction of a second.
    assert!(
        hpc.nanos < 1_000_000_000,
        "nanos out of range: {}",
        hpc.nanos
    );

    // Read a consistent (same-block) snapshot of both sysvars. A slot boundary
    // may land between the two reads, so retry until they agree.
    let mut consistent = false;
    for _ in 0..50 {
        let clock = read_clock(&env);
        hpc = read_high_precision_clock(&env);
        if hpc.unix_timestamp == clock.unix_timestamp {
            consistent = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        consistent,
        "HighPrecisionClock.unix_timestamp never matched Clock.unix_timestamp"
    );
    assert!(hpc.nanos < 1_000_000_000);
}
