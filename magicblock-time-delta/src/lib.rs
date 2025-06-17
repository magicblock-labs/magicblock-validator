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

    /// Get number of complete tick intervals that have elapsed
    pub fn tick_count(&self) -> u64 {
        if self.tick_interval == 0 {
            return 0;
        }
        self.elapsed_ms() / self.tick_interval
    }

    /// Get remaining time until next tick in milliseconds
    pub fn time_to_next_tick(&self) -> u64 {
        if self.tick_interval == 0 {
            return 0;
        }
        let elapsed = self.elapsed_ms();
        let remainder = elapsed % self.tick_interval;
        if remainder == 0 && elapsed > 0 {
            0 // Exactly on a tick boundary
        } else {
            self.tick_interval - remainder
        }
    }
}

/// Solana-specific time delta implementation using Clock sysvar
#[derive(Debug, Clone)]
pub struct SolanaTimeDelta {
    last_slot: u64,
    last_unix_timestamp: i64,
    tick_interval_ms: u64,
}

impl SolanaTimeDelta {
    /// Create a new SolanaTimeDelta tracker with specified tick interval
    pub fn new(tick_interval_ms: u64) -> Self {
        Self {
            last_slot: 0,
            last_unix_timestamp: 0,
            tick_interval_ms,
        }
    }

    /// Get the tick interval in milliseconds
    pub fn tick_interval(&self) -> u64 {
        self.tick_interval_ms
    }

    /// Get the last recorded slot
    pub fn last_slot(&self) -> u64 {
        self.last_slot
    }

    /// Get the last recorded unix timestamp
    pub fn last_unix_timestamp(&self) -> i64 {
        self.last_unix_timestamp
    }

    /// Update with current Solana clock
    pub fn update_with_clock(&mut self, slot: u64, unix_timestamp: i64) {
        self.last_slot = slot;
        self.last_unix_timestamp = unix_timestamp;
    }

    /// Get elapsed time in milliseconds based on Solana clock
    pub fn elapsed_ms_with_clock(&self, current_unix_timestamp: i64) -> u64 {
        let elapsed_seconds = current_unix_timestamp.saturating_sub(self.last_unix_timestamp);
        (elapsed_seconds * 1000).max(0) as u64
    }

    /// Check if enough time has passed based on Solana clock
    pub fn should_tick_with_clock(&self, current_unix_timestamp: i64) -> bool {
        self.elapsed_ms_with_clock(current_unix_timestamp) >= self.tick_interval_ms
    }

    /// Get tick count based on Solana clock
    pub fn tick_count_with_clock(&self, current_unix_timestamp: i64) -> u64 {
        if self.tick_interval_ms == 0 {
            return 0;
        }
        self.elapsed_ms_with_clock(current_unix_timestamp) / self.tick_interval_ms
    }

    /// Get remaining time until next tick based on Solana clock
    pub fn time_to_next_tick_with_clock(&self, current_unix_timestamp: i64) -> u64 {
        if self.tick_interval_ms == 0 {
            return 0;
        }
        let elapsed = self.elapsed_ms_with_clock(current_unix_timestamp);
        let remainder = elapsed % self.tick_interval_ms;
        if remainder == 0 && elapsed > 0 {
            0 // Exactly on a tick boundary
        } else {
            self.tick_interval_ms - remainder
        }
    }
}

/// Helper functions for Solana integration
#[cfg(feature = "solana-integration")]
pub mod solana_helpers {
    use super::SolanaTimeDelta;
    use solana_program::{clock::Clock, sysvar::Sysvar};

    /// Create and update a SolanaTimeDelta with current Clock sysvar
    pub fn update_with_current_clock(delta: &mut SolanaTimeDelta) -> Result<(), Box<dyn std::error::Error>> {
        let clock = Clock::get()?;
        delta.update_with_clock(clock.slot, clock.unix_timestamp);
        Ok(())
    }

    /// Check if a SolanaTimeDelta should tick using current Clock sysvar
    pub fn should_tick_now(delta: &SolanaTimeDelta) -> Result<bool, Box<dyn std::error::Error>> {
        let clock = Clock::get()?;
        Ok(delta.should_tick_with_clock(clock.unix_timestamp))
    }

    /// Get tick count using current Clock sysvar
    pub fn tick_count_now(delta: &SolanaTimeDelta) -> Result<u64, Box<dyn std::error::Error>> {
        let clock = Clock::get()?;
        Ok(delta.tick_count_with_clock(clock.unix_timestamp))
    }
}

// Export physics module
pub mod physics;

// Re-export commonly used physics types for convenience
pub use physics::{PlayerState, PhysicsWorld};

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

    #[test]
    fn test_tick_count() {
        let mut delta = TimeDelta::new(50);
        
        // Initially, no ticks should have occurred
        assert_eq!(delta.tick_count(), 0);
        
        // Simulate passage of time
        thread::sleep(Duration::from_millis(120));
        
        // Two ticks should have occurred
        assert_eq!(delta.tick_count(), 2);
        
        // Update and check tick count
        delta.update();
        assert_eq!(delta.tick_count(), 0);
        
        // Wait for one tick interval
        thread::sleep(Duration::from_millis(60));
        assert_eq!(delta.tick_count(), 1);
    }

    #[test]
    fn test_tick_count_zero_interval() {
        let delta = TimeDelta::new(0);
        
        // With zero interval, tick count should always be 0
        assert_eq!(delta.tick_count(), 0);
        
        thread::sleep(Duration::from_millis(10));
        assert_eq!(delta.tick_count(), 0);
    }

    #[test]
    fn test_time_to_next_tick() {
        let mut delta = TimeDelta::new(50);
        
        // Initially, time to next tick should be close to tick interval
        let initial_time_to_tick = delta.time_to_next_tick();
        assert!(initial_time_to_tick <= 50);
        assert!(initial_time_to_tick > 40); // Should be close to 50ms
        
        // Simulate passage of time
        thread::sleep(Duration::from_millis(30));
        
        // Time to next tick should decrease
        let time_to_tick = delta.time_to_next_tick();
        assert!(time_to_tick < initial_time_to_tick);
        assert!(time_to_tick <= 20);
        
        // Update and check time to next tick
        delta.update();
        let reset_time_to_tick = delta.time_to_next_tick();
        assert!(reset_time_to_tick <= 50);
        assert!(reset_time_to_tick > 40);
    }

    #[test]
    fn test_time_to_next_tick_zero_interval() {
        let delta = TimeDelta::new(0);
        
        // With zero interval, time to next tick should always be 0
        assert_eq!(delta.time_to_next_tick(), 0);
        
        thread::sleep(Duration::from_millis(10));
        assert_eq!(delta.time_to_next_tick(), 0);
    }

    #[test]
    fn test_tick_boundary_behavior() {
        let delta = TimeDelta::new(25);
        
        // Wait for more than one tick interval
        thread::sleep(Duration::from_millis(30));
        
        // Should have at least one tick
        assert!(delta.should_tick());
        assert!(delta.tick_count() >= 1);
        
        // Check timing details
        let elapsed = delta.elapsed_ms();
        let time_to_next = delta.time_to_next_tick();
        let tick_count = delta.tick_count();
        
        println!("Elapsed: {}ms, Tick count: {}, Time to next: {}ms", elapsed, tick_count, time_to_next);
        
        // Time to next should be reasonable
        assert!(time_to_next < 25);
        assert!(time_to_next + (elapsed % 25) == 25 || time_to_next == 0);
        
        // Wait for more time
        thread::sleep(Duration::from_millis(30));
        let new_tick_count = delta.tick_count();
        assert!(new_tick_count > tick_count);
    }

    // Solana-specific tests
    #[test]
    fn test_solana_time_delta_creation() {
        let solana_delta = SolanaTimeDelta::new(100);
        assert_eq!(solana_delta.tick_interval(), 100);
        assert_eq!(solana_delta.last_slot(), 0);
        assert_eq!(solana_delta.last_unix_timestamp(), 0);
    }

    #[test]
    fn test_solana_update_with_clock() {
        let mut solana_delta = SolanaTimeDelta::new(50);
        
        // Initial state
        assert_eq!(solana_delta.last_slot(), 0);
        assert_eq!(solana_delta.last_unix_timestamp(), 0);
        
        // Update with clock data
        solana_delta.update_with_clock(12345, 1640995200); // Jan 1, 2022
        assert_eq!(solana_delta.last_slot(), 12345);
        assert_eq!(solana_delta.last_unix_timestamp(), 1640995200);
    }

    #[test]
    fn test_solana_elapsed_ms_with_clock() {
        let mut solana_delta = SolanaTimeDelta::new(30);
        
        // Set initial time
        let start_time = 1640995200; // Jan 1, 2022
        solana_delta.update_with_clock(100, start_time);
        
        // Check elapsed time with different timestamps
        assert_eq!(solana_delta.elapsed_ms_with_clock(start_time), 0);
        assert_eq!(solana_delta.elapsed_ms_with_clock(start_time + 5), 5000); // 5 seconds = 5000ms
        assert_eq!(solana_delta.elapsed_ms_with_clock(start_time + 10), 10000); // 10 seconds = 10000ms
        
        // Test negative time (should not happen but be safe)
        assert_eq!(solana_delta.elapsed_ms_with_clock(start_time - 5), 0);
    }

    #[test]
    fn test_solana_should_tick_with_clock() {
        let mut solana_delta = SolanaTimeDelta::new(5000); // 5 second intervals
        
        let start_time = 1640995200;
        solana_delta.update_with_clock(100, start_time);
        
        // Should not tick immediately
        assert!(!solana_delta.should_tick_with_clock(start_time));
        
        // Should not tick after 3 seconds
        assert!(!solana_delta.should_tick_with_clock(start_time + 3));
        
        // Should tick after 5 seconds
        assert!(solana_delta.should_tick_with_clock(start_time + 5));
        
        // Should tick after 10 seconds
        assert!(solana_delta.should_tick_with_clock(start_time + 10));
    }

    #[test]
    fn test_solana_tick_count_with_clock() {
        let mut solana_delta = SolanaTimeDelta::new(2000); // 2 second intervals
        
        let start_time = 1640995200;
        solana_delta.update_with_clock(100, start_time);
        
        // No ticks initially
        assert_eq!(solana_delta.tick_count_with_clock(start_time), 0);
        
        // One tick after 2 seconds
        assert_eq!(solana_delta.tick_count_with_clock(start_time + 2), 1);
        
        // Two ticks after 4 seconds
        assert_eq!(solana_delta.tick_count_with_clock(start_time + 4), 2);
        
        // Three ticks after 7 seconds (7/2 = 3)
        assert_eq!(solana_delta.tick_count_with_clock(start_time + 7), 3);
    }

    #[test]
    fn test_solana_tick_count_zero_interval() {
        let solana_delta = SolanaTimeDelta::new(0);
        
        // With zero interval, tick count should always be 0
        assert_eq!(solana_delta.tick_count_with_clock(1640995200), 0);
        assert_eq!(solana_delta.tick_count_with_clock(1640995210), 0);
    }

    #[test]
    fn test_solana_time_to_next_tick_with_clock() {
        let mut solana_delta = SolanaTimeDelta::new(3000); // 3 second intervals
        
        let start_time = 1640995200;
        solana_delta.update_with_clock(100, start_time);
        
        // Initially, time to next tick should be the full interval
        assert_eq!(solana_delta.time_to_next_tick_with_clock(start_time), 3000);
        
        // After 1 second, should have 2 seconds remaining
        assert_eq!(solana_delta.time_to_next_tick_with_clock(start_time + 1), 2000);
        
        // After 2 seconds, should have 1 second remaining
        assert_eq!(solana_delta.time_to_next_tick_with_clock(start_time + 2), 1000);
        
        // After exactly 3 seconds, should be at tick boundary
        assert_eq!(solana_delta.time_to_next_tick_with_clock(start_time + 3), 0);
        
        // After 4 seconds, should have 2 seconds until next tick
        assert_eq!(solana_delta.time_to_next_tick_with_clock(start_time + 4), 2000);
    }

    #[test]
    fn test_solana_time_to_next_tick_zero_interval() {
        let solana_delta = SolanaTimeDelta::new(0);
        
        // With zero interval, time to next tick should always be 0
        assert_eq!(solana_delta.time_to_next_tick_with_clock(1640995200), 0);
        assert_eq!(solana_delta.time_to_next_tick_with_clock(1640995210), 0);
    }

    #[test]
    fn test_solana_integration_scenario() {
        let mut solana_delta = SolanaTimeDelta::new(10000); // 10 second intervals
        
        // Simulate blockchain progression
        let mut current_slot = 1000;
        let mut current_time = 1640995200;
        
        // Initial setup
        solana_delta.update_with_clock(current_slot, current_time);
        assert_eq!(solana_delta.tick_count_with_clock(current_time), 0);
        
        // Simulate 5 seconds passing
        current_time += 5;
        current_slot += 20; // Assume ~4 slots per second
        assert!(!solana_delta.should_tick_with_clock(current_time));
        assert_eq!(solana_delta.tick_count_with_clock(current_time), 0);
        assert_eq!(solana_delta.time_to_next_tick_with_clock(current_time), 5000);
        
        // Simulate 10 seconds total passing
        current_time += 5;
        current_slot += 20;
        assert!(solana_delta.should_tick_with_clock(current_time));
        assert_eq!(solana_delta.tick_count_with_clock(current_time), 1);
        assert_eq!(solana_delta.time_to_next_tick_with_clock(current_time), 0);
        
        // Update to new time and continue
        solana_delta.update_with_clock(current_slot, current_time);
        assert_eq!(solana_delta.tick_count_with_clock(current_time), 0);
        
        // Simulate 25 seconds passing from new baseline
        current_time += 25;
        assert_eq!(solana_delta.tick_count_with_clock(current_time), 2); // 25/10 = 2
    }
}
