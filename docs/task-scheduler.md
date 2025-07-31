# Task Scheduler API

## Overview
The task scheduler allows developers to schedule transactions for periodic execution on the MagicBlock validator.

## Usage Examples

### Scheduling a Periodic Task
```rust
use magicblock_program::magicblock_instruction;

let transaction = magicblock_instruction::schedule_task(
    &payer,
    task_id,
    TaskTrigger::Periodic(start_time, interval_microseconds),
    vec![unsigned_transaction],
    remaining_budget,
    recent_blockhash,
);
```

### Canceling a Task
```rust
let transaction = magicblock_instruction::cancel_task(
    &authority,
    task_id,
    recent_blockhash,
);
```

### Updating a Task
```rust
let transaction = magicblock_instruction::update_task(
    &authority,
    task_id,
    Some(new_trigger),
    Some(new_transactions),
    Some(new_budget),
    recent_blockhash,
);
```

## Architecture

### Components

1. **TaskContext Account**: Stores tasks and cancellation requests on-chain
2. **Task Scheduler Service**: Runs alongside the validator to execute scheduled tasks
3. **Database**: SQLite database for efficient task storage and retrieval
4. **Geyser Integration**: Monitors TaskContext account changes

### Data Flow

1. User schedules task via program instruction
2. Task is stored in TaskContext account
3. Task Scheduler Service monitors TaskContext via Geyser
4. Service adds task to local database
5. Service executes tasks at scheduled intervals
6. Service updates task state after execution

## Configuration

The task scheduler can be configured via the validator configuration:

```toml
[task_scheduler]
db_path = "/path/to/scheduler.db"
tick_interval_ms = 100
max_tasks_per_tick = 10
```

## Security Considerations

- Only task authorities can cancel their own tasks
- Database is protected by file system permissions

## Performance Considerations

- Database is indexed for efficient task retrieval
- Tasks are executed in batches to minimize overhead
- Failed task executions are logged but don't block other tasks
- Database operations are optimized for high-frequency access 