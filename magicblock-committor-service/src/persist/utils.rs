use std::time::{SystemTime, UNIX_EPOCH};

/// Fits a u64 into an i64, by mapping the range [0, i64::MAX] to itself, and
/// mapping the range [i64::MAX + 1, u64::MAX - 1] into the negative range of i64.
/// NOTE: this fails for u64::MAX
/// TODO(edwin): just store bit-copy in i64
pub(crate) fn u64_into_i64(n: u64) -> i64 {
    if n > i64::MAX as u64 {
        -((n - i64::MAX as u64) as i64)
    } else {
        n as i64
    }
}

/// Extracts a u64 that was fitted into an i64 by `u64_into_i64`.
pub(crate) fn i64_into_u64(n: i64) -> u64 {
    if n < 0 {
        n.unsigned_abs() + i64::MAX as u64
    } else {
        n as u64
    }
}

/// Gets the current timestamp in seconds since the Unix epoch
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(u: u64) {
        let i = u64_into_i64(u);
        let u2 = i64_into_u64(i);
        assert_eq!(u, u2);
    }

    #[test]
    fn test_u64_i64_conversion_via_round_trip() {
        round_trip(0);
        round_trip(1);
        round_trip(i64::MAX as u64);
        round_trip(i64::MAX as u64 + 1);

        // NOTE: the below which points out that we cannot round trip u64::MAX,
        assert_eq!(i64::MAX as u64 * 2 + 1, u64::MAX);

        // This is the largest we can roundtrip
        round_trip(u64::MAX - 1);
        round_trip(i64::MAX as u64 * 2);

        // This would fail:
        // round_trip(u64::MAX);
    }
}
