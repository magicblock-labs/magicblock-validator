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
[task-scheduler]
reset = false
min-interval = "10ms"
failed-task-retention = "7d"
failed-task-cleanup-interval = "1h"
```

Failed task execution records and failed scheduling records older than
`failed-task-retention` are deleted every `failed-task-cleanup-interval`.

## Security Considerations

- Only task authorities can cancel their own tasks
- Database is protected by file system permissions

## Performance Considerations

- Same-tick delay-queue draining; crank sends parallelize `send_transaction` (consider bounding concurrency under heavy load).
- `Arc` for stored instructions; configurable `RpcSendTransactionConfig` for crank sends.
- SQLite: WAL journal, `NORMAL` synchronous mode, enlarged page cache; after each crank RPC batch completes, success/failure persistence uses one transaction (`apply_crank_batch_completion`) instead of one commit per task.
