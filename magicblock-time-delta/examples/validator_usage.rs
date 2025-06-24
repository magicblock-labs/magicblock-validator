//! Practical examples demonstrating time delta usage in Solana validator context

use magicblock_time_delta::{TimeDelta, SolanaTimeDelta};
use std::time::Duration;
use std::thread;

/// Example: Validator health monitoring with periodic checks
struct ValidatorMonitor {
    health_check_timer: TimeDelta,
    metrics_timer: TimeDelta,
}

impl ValidatorMonitor {
    pub fn new() -> Self {
        Self {
            health_check_timer: TimeDelta::new(5000),  // Health check every 5 seconds
            metrics_timer: TimeDelta::new(30000),      // Metrics collection every 30 seconds  
        }
    }

    pub fn process_periodic_tasks(&mut self) {
        // Check if health monitoring should run
        if self.health_check_timer.should_tick() {
            let tick_count = self.health_check_timer.tick_count();
            println!("Running health check (missed {} ticks)", tick_count);
            self.health_check_timer.update();
        }

        // Check if metrics collection should run
        if self.metrics_timer.should_tick() {
            println!("Collecting validator metrics");
            self.metrics_timer.update();
        }
    }
}

/// Example: Rate limiter for RPC requests using time deltas
struct RpcRateLimiter {
    request_timer: TimeDelta,
    max_requests_per_interval: u64,
    current_requests: u64,
}

impl RpcRateLimiter {
    pub fn new(requests_per_second: u64) -> Self {
        Self {
            request_timer: TimeDelta::new(1000), // 1 second intervals
            max_requests_per_interval: requests_per_second,
            current_requests: 0,
        }
    }

    pub fn allow_request(&mut self) -> bool {
        // Reset counter if interval has passed
        if self.request_timer.should_tick() {
            self.current_requests = 0;
            self.request_timer.update();
        }

        // Check if we're under the limit
        if self.current_requests < self.max_requests_per_interval {
            self.current_requests += 1;
            true
        } else {
            false
        }
    }
}

fn main() {
    println!("=== Magicblock Validator Time Delta Examples ===\n");

    // Example 1: Validator monitoring
    println!("1. Validator Health Monitoring:");
    let mut monitor = ValidatorMonitor::new();
    
    // Simulate validator running for a short time
    for i in 0..8 {
        monitor.process_periodic_tasks();
        thread::sleep(Duration::from_millis(1000)); // Simulate 1 second passing
        
        if i == 4 {
            println!("  [Simulating 5 second delay...]");
            thread::sleep(Duration::from_millis(4000)); // Simulate longer delay
        }
    }

    println!("\n{}\n", "=".repeat(50));

    // Example 2: Rate limiting
    println!("2. RPC Rate Limiting (3 requests/second):");
    let mut rate_limiter = RpcRateLimiter::new(3);
    
    // Simulate 8 rapid requests
    for i in 1..=8 {
        if rate_limiter.allow_request() {
            println!("  Request {} - ALLOWED", i);
        } else {
            println!("  Request {} - DENIED", i);
        }
        thread::sleep(Duration::from_millis(200));
    }

    println!("\n{}\n", "=".repeat(50));

    // Example 3: Blockchain timing
    println!("3. Blockchain Slot Timing:");
    let mut slot_timer = SolanaTimeDelta::new(400); // 400ms expected slot time
    
    let mut current_slot = 1000;
    let mut current_time = 1640995200;
    
    slot_timer.update_with_clock(current_slot, current_time);
    
    for _i in 1..=3 {
        current_slot += 1;
        current_time += 1; // 1 second passed
        
        let elapsed = slot_timer.elapsed_ms_with_clock(current_time);
        let should_tick = slot_timer.should_tick_with_clock(current_time);
        
        println!("  Slot {}: {}ms elapsed, should_tick: {}", 
                current_slot, elapsed, should_tick);
        
        thread::sleep(Duration::from_millis(100));
    }

    println!("\n=== Examples Complete ===");
}
