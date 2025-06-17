# Magicblock Time Delta

A high-precision time delta tracking library for the Magicblock Solana validator, designed for cron jobs, real-time applications, and blockchain timing operations.

## Features

- **High-precision timing**: Millisecond-accurate time delta measurements
- **Tick counting**: Calculate exact number of elapsed intervals
- **Blockchain integration**: Solana Clock sysvar support for on-chain timing
- **Rate limiting**: Built-in support for request throttling
- **Cron scheduling**: Framework for periodic task execution
- **Zero-cost abstractions**: Efficient implementations with minimal overhead

## Core Types

### `TimeDelta`
System time-based delta tracking for real-time applications:
```rust
use magicblock_time_delta::TimeDelta;

let mut timer = TimeDelta::new(1000); // 1 second intervals

// Check if enough time has passed
if timer.should_tick() {
    let ticks = timer.tick_count(); // How many intervals elapsed
    println!("Processing {} ticks", ticks);
    timer.update(); // Reset the timer
}
```

### `SolanaTimeDelta`
Blockchain time-based delta tracking using Solana's Clock sysvar:
```rust
use magicblock_time_delta::SolanaTimeDelta;

let mut blockchain_timer = SolanaTimeDelta::new(5000); // 5 second intervals

// Update with current blockchain time
blockchain_timer.update_with_clock(slot, unix_timestamp);

// Check timing based on blockchain clock
if blockchain_timer.should_tick_with_clock(current_timestamp) {
    // Execute blockchain-based logic
}
```

## Use Cases

### Validator Health Monitoring
```rust
let mut health_check = TimeDelta::new(5000); // Every 5 seconds

if health_check.should_tick() {
    check_validator_health();
    health_check.update();
}
```

### Rate Limiting RPC Requests
```rust
let mut rate_limiter = TimeDelta::new(1000); // 1 second windows
let mut request_count = 0;

if rate_limiter.should_tick() {
    request_count = 0; // Reset counter
    rate_limiter.update();
}

if request_count < MAX_REQUESTS_PER_SECOND {
    // Allow request
    request_count += 1;
} else {
    // Deny request
}
```

## Examples

Run the included examples to see time delta usage in validator contexts:

```bash
# Validator monitoring and rate limiting examples
cargo run --example validator_usage

# Cron scheduler for maintenance tasks  
cargo run --example cron_scheduler
```

## Testing

All timing functionality is thoroughly tested with real-time verification:

```bash
cargo test
```