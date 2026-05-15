# Task Scheduler API

## Architecture

### Components

1. **TaskContext Account**: Stores tasks and cancellation requests on-chain
2. **Task Scheduler Service**: Runs alongside the validator to execute scheduled tasks
3. **Database**: SQLite database for efficient task storage and retrieval
4. **Geyser Integration**: Monitors TaskContext account changes

### Data Flow

1. User schedules task via program instruction
2. Task is stored in TaskContext account
3. Task Scheduler Service monitors TaskContext periodically
4. Service adds task to local database
5. Service executes tasks at scheduled intervals
6. Service updates task state after execution

## Configuration

The task scheduler can be configured via the validator configuration:

```toml
[task_scheduler]
reset = false
min-interval = "10ms"
```

## Security Considerations

- Only task authorities can cancel their own tasks
- Database is protected by file system permissions

## Performance Considerations

- SQLite uses WAL mode, `synchronous = NORMAL`, and a busy timeout to reduce lock contention.
- When multiple tasks expire on the same timer tick, the service drains the delay queue and processes them in one batch pass.
- Instruction payloads are stored behind `Arc` to avoid cloning large instruction lists on every fire.
- For lower latency than loopback RPC, a deployment can construct `TaskSchedulerService::with_submitter` with a custom `CrankTransactionSubmitter` (for example wiring into the validator transaction pipeline).
- Failed task executions are retried (with backoff) or moved to a failed table; they do not block the scheduler loop indefinitely.
