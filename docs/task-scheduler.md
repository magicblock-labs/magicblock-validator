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
3. Task Scheduler Service monitors TaskContext via Geyser
4. Service adds task to local database
5. Service executes tasks at scheduled intervals
6. Service updates task state after execution

## Configuration

The task scheduler can be configured via the validator configuration:

```toml
[task_scheduler]
db_path = "/path/to/scheduler.db"
reset_db = false
millis_per_tick = 100
```

## Security Considerations

- Only task authorities can cancel their own tasks
- Database is protected by file system permissions

## Performance Considerations

- Database is indexed for efficient task retrieval
- Tasks are executed in batches to minimize overhead
- Failed task executions are logged but don't block other tasks
- Database operations are optimized for high-frequency access 