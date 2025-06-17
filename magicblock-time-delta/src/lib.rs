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
            .expect("Time went backwards")
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

    /// Get elapsed time since last update in milliseconds
    pub fn elapsed_ms(&self) -> u64 {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;
        
        current_time.saturating_sub(self.last_update)
    }

    /// Check if at least one tick interval has elapsed
    pub fn should_tick(&self) -> bool {
        self.elapsed_ms() >= self.tick_interval
    }

    /// Update the last update time to current time
    pub fn update(&mut self) {
        self.last_update = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

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
        
        assert_eq!(delta.tick_interval, tick_interval);
        assert!(delta.last_update > 0);
    }

    #[test]
    fn test_different_intervals() {
        let delta1 = TimeDelta::new(5);
        let delta2 = TimeDelta::new(1000);
        
        assert_eq!(delta1.tick_interval(), 5);
        assert_eq!(delta2.tick_interval(), 1000);
    }

    #[test]
    fn test_elapsed_ms() {
        let delta = TimeDelta::new(50);
        
        // Elapsed time should be very small immediately after creation
        assert!(delta.elapsed_ms() < 10);
        
        // Simulate passage of time
        thread::sleep(Duration::from_millis(20));
        
        // Now elapsed time should be at least 20ms
        let elapsed = delta.elapsed_ms();
        assert!(elapsed >= 20);
        assert!(elapsed < 100); // Should be reasonable
    }

    #[test]
    fn test_should_tick() {
        let delta = TimeDelta::new(30);
        
        // Should not tick immediately after creation
        assert!(!delta.should_tick());
        
        // Simulate passage of time less than tick interval
        thread::sleep(Duration::from_millis(10));
        assert!(!delta.should_tick());
        
        // Simulate passage of time greater than tick interval
        thread::sleep(Duration::from_millis(25));
        assert!(delta.should_tick());
    }

    #[test]
    fn test_update() {
        let mut delta = TimeDelta::new(50);
        let initial_time = delta.last_update();
        
        // Wait a bit and update
        thread::sleep(Duration::from_millis(20));
        delta.update();
        
        // Last update time should have changed
        assert!(delta.last_update() > initial_time);
        
        // Elapsed time should be minimal after update
        assert!(delta.elapsed_ms() < 10);
        
        // Should not tick immediately after update
        assert!(!delta.should_tick());
    }

    #[test]
    fn test_update_resets_timing() {
        let mut delta = TimeDelta::new(30);
        
        // Wait enough time to tick
        thread::sleep(Duration::from_millis(35));
        assert!(delta.should_tick());
        
        // Update should reset the timing
        delta.update();
        assert!(!delta.should_tick());
        assert!(delta.elapsed_ms() < 10);
    }
}
