use std::time::{SystemTime, UNIX_EPOCH};

/// Time delta tracker for measuring elapsed time between operations
#[derive(Debug, Clone)]
pub struct TimeDelta {
    last_update: u64,      // Unix timestamp in milliseconds
    tick_interval: u64,    // Expected interval between ticks in milliseconds
}

impl TimeDelta {
    /// Create a new TimeDelta tracker with specified tick interval
    pub fn new(tick_interval_ms: u64) -> Self {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        
        Self {
            last_update: current_time,
            tick_interval: tick_interval_ms,
        }
    }

    /// Get the tick interval in milliseconds
    pub fn tick_interval(&self) -> u64 {
        self.tick_interval
    }

    /// Get the last update timestamp in milliseconds
    pub fn last_update(&self) -> u64 {
        self.last_update
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_delta_creation() {
        let delta = TimeDelta::new(10);
        assert_eq!(delta.tick_interval(), 10);
        assert!(delta.last_update() > 0);
    }

    #[test]
    fn test_time_delta_fields() {
        let tick_interval = 100;
        let delta = TimeDelta::new(tick_interval);
        
        assert_eq!(delta.tick_interval(), tick_interval);
        
        // Verify that the last_update is a reasonable timestamp (after year 2020)
        let min_timestamp = 1577836800000; // Jan 1, 2020 in milliseconds
        assert!(delta.last_update() > min_timestamp);
    }
}
