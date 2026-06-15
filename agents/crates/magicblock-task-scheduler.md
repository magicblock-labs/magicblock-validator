# `magicblock-task-scheduler`

## Purpose

`magicblock-task-scheduler` is the validator-side service that turns Magic Program scheduled-task requests into recurring local crank transactions. Programs schedule or cancel tasks during normal ER execution; the processor forwards those `TaskRequest`s to this crate, which persists them in SQLite, delays them until their next execution time, and submits validator-signed crank transactions back through the validator RPC endpoint.

High-level responsibilities:

- persist scheduled task definitions and failure records in `task_scheduler.sqlite`;
- load and reschedule persisted tasks when the primary validator starts;
- receive `ScheduleTaskRequest` and `CancelTaskRequest` values from the transaction executor channel;
- maintain an in-memory `DelayQueue` for due tasks, including replacement/cancellation state;
- submit crank transactions that call Magic Program `ExecuteTask` with validator authority and crank signer accounts;
- record execution success, final completion, retryable failures, permanent failures, and failed scheduling records;
- periodically clean up old failed scheduling/execution records according to validator config.

This crate sits on the scheduled-task/crank path and can affect transaction execution latency indirectly by how quickly it drains executor-produced task requests and how much RPC/SQLite work it performs. It is also persistence-sensitive: task definitions and failure records survive restart unless `task_scheduler.reset` is enabled.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-task-scheduler` change. In particular, update it for changes to:

- public exports in `src/lib.rs`, `SchedulerDatabase`, `TaskSchedulerService`, or `TaskSchedulerError`;
- SQLite schema, database path, WAL/PRAGMA settings, retention behavior, optimistic `updated_at` concurrency, or restart recovery semantics;
- scheduling/cancellation authorization, interval clamping, iteration handling, retry/backoff policy, or stale completion handling;
- crank transaction layout, signer requirements, Magic Program instruction construction, blockhash source, send configuration, or RPC endpoint selection;
- startup/shutdown wiring in `magicblock-api`, primary/replica gating, cancellation handling, or draining of in-flight crank completions;
- `TaskSchedulerConfig` fields/defaults/env keys or README configuration examples;
- task scheduler unit tests, integration-test setup, or validation commands.

Because this crate consumes task requests emitted by Magic Program execution, also update this file when `magicblock-program`, `magicblock-magic-program-api`, `magicblock-core`, or `magicblock-processor` changes `ScheduleTask`, `CancelTask`, `ExecuteTask`, `TaskRequest`, `ExecutionTlsStash`, or the scheduled-task channel semantics.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-task-scheduler/Cargo.toml` | Package metadata and dependencies on config, core channels, ledger latest blockhash, Magic Program instruction helpers, Solana RPC/transaction crates, Tokio, and SQLite. |
| `magicblock-task-scheduler/README.md` | Operator-facing overview, `[task-scheduler]` config example, and performance notes. Keep it aligned with config and service behavior. |
| `magicblock-task-scheduler/src/lib.rs` | Public crate surface. Re-exports `SchedulerDatabase`, `TaskSchedulerError`, and `TaskSchedulerService`. |
| `magicblock-task-scheduler/src/db.rs` | SQLite persistence layer, task/failure record types, task serialization, optimistic concurrency tokens, and batch crank-completion transaction. |
| `magicblock-task-scheduler/src/service.rs` | Runtime service, startup recovery, request processing, delay queue, crank send batching, retry/backoff, cleanup ticker, and cancellation handling. |
| `magicblock-task-scheduler/src/errors.rs` | `TaskSchedulerError` and `TaskSchedulerResult`. Wraps SQLite, bincode, RPC, I/O, invalid config, and unauthorized replacement errors. |
| `magicblock-config/src/config/scheduler.rs` | `TaskSchedulerConfig` (`reset`, `min_interval`, failed-record retention and cleanup interval). |
| `magicblock-api/src/magic_validator.rs` | Constructs the service at startup and starts it only after the validator leaves `StartingUp` in primary mode. |
| `magicblock-core/src/link/transactions.rs` | Defines `ScheduledTasksTx`/`ScheduledTasksRx`, the channel carrying `TaskRequest`s from executor to service. |
| `magicblock-processor/src/executor/processing.rs` | Drains `ExecutionTlsStash` after transaction execution and sends scheduled-task requests to this crate. |
| `programs/magicblock/src/schedule_task/` | Magic Program processors that validate schedule/cancel/execute-task instructions and register `TaskRequest`s. |
| `test-integration/test-task-scheduler/` | Integration tests for scheduling, cancellation, rescheduling, signing, schedule errors, unauthorized reschedule, crank signer use, and scheduled-commit interaction. |

Main consumers:

- `magicblock-api`, which owns construction/startup and passes the local aperture HTTP URL as the RPC endpoint;
- `magicblock-processor`, which sends task requests after successful instruction execution through `ScheduledTasksTx`;
- Magic Program task instructions, which define the wire/request semantics this crate persists and executes;
- task scheduler integration tests, which inspect the SQLite database directly through `SchedulerDatabase`.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exposes:

- `pub mod db`, `pub mod errors`, and `pub mod service`;
- `pub use db::SchedulerDatabase`;
- `pub use errors::TaskSchedulerError`;
- `pub use service::TaskSchedulerService`.

### `SchedulerDatabase`

`SchedulerDatabase` wraps a single `rusqlite::Connection` in `Arc<tokio::sync::Mutex<_>>`. It is cloneable, but all DB operations serialize through that mutex.

Important API:

- `SchedulerDatabase::path(path)` returns `path.join("task_scheduler.sqlite")`;
- `new(path)` opens SQLite, enables WAL, `synchronous=NORMAL`, `busy_timeout=5000`, and a larger page cache, then creates `tasks`, `failed_scheduling`, and `failed_tasks` tables if missing;
- `insert_task`, `get_task`, `get_tasks`, `get_task_ids`, `remove_task`, and `unschedule_task` manage scheduled task rows;
- `insert_failed_scheduling`, `insert_failed_task`, `get_failed_schedulings`, and `get_failed_tasks` manage diagnostic failure records;
- `apply_crank_batch_completion(...)` atomically applies one batch of success updates, success removals, failed moves, and retry checks using optimistic `tasks.updated_at` tokens;
- `delete_failed_records_older_than(cutoff)` removes old rows from both failure tables in one transaction.

`DbTask` is the persisted runtime task shape. It stores task IDs and timestamps as `i64`, serializes `Vec<Instruction>` with `bincode`, stores authority as a stringified `Pubkey`, and uses `executions_left`, `last_execution_millis`, and `updated_at` to drive future scheduling.

### `TaskSchedulerService`

`TaskSchedulerService::new(path, config, rpc_url, scheduled_tasks, block, slot_interval, token)` creates the service. It may remove the DB file first when `config.reset` is true, validates that `config.min_interval` fits in `u32::MAX` milliseconds, opens the database, and constructs a nonblocking Solana `RpcClient` pointed at `rpc_url`.

`start(self)` loads persisted tasks and spawns the main Tokio task, returning `JoinHandle<TaskSchedulerResult<()>>`.

Internally the service owns:

- the SQLite database;
- a `ScheduledTasksRx` channel from the processor;
- a `LatestBlock` handle for crank transaction blockhashes;
- a `DelayQueue<DbTask>` plus `task_queue_keys` for cancellation/replacement;
- `task_versions` keyed by task ID to reject stale in-flight completions;
- retry counters and exponential backoff state;
- a cancellation token and failed-record cleanup interval;
- an `AtomicU64` counter used to create unique noop instructions in crank transactions.

The type has manual `unsafe impl Send`/`Sync` with an explicit safety comment: the service is moved into one Tokio task by `start()` and is not cloned. Do not make it shared/mutated from multiple tasks without revisiting this assumption.

### Errors

`TaskSchedulerError` wraps invalid configuration, SQLite, bincode, Solana RPC client errors, I/O, unauthorized task replacement, and a currently unused `SizeMismatch` variant. Schedule/cancel processing errors are normally recorded as failed scheduling and treated as recoverable; service-level failures returned from the main loop cause `magicblock-api` to log and exit the process.

## Runtime flows

### Startup and primary-mode gating

```text
MagicValidator::try_new
  -> SchedulerDatabase::path(storage parent)
  -> TaskSchedulerService::new(..., aperture HTTP URL, ScheduledTasksRx, LatestBlock, slot interval)
MagicValidator::start
  -> wait until CoordinationMode != StartingUp
  -> if Primary: tokio::spawn(task_scheduler.start())
  -> if Replica: do not start task scheduler
```

On `start()`, `load_persisted_tasks` reads all rows from `tasks`, removes invalid rows (`execution_interval_millis <= 0`, `>= u32::MAX`, or `executions_left <= 0`), and inserts valid rows into the delay queue. Restarted tasks are delayed until the later of their next scheduled time and two slot intervals. That two-slot minimum avoids cranking before the validator has produced a fresh blockhash after restart.

### Schedule request flow

```text
Magic Program ScheduleTask instruction
  -> ExecutionTlsStash::register_task(TaskRequest::Schedule)
  -> executor process_scheduled_tasks sends over ScheduledTasksTx
  -> TaskSchedulerService::process_schedule_request
  -> SQLite upsert + DelayQueue insert
```

Processing details:

1. Invalid intervals are ignored by the service. The Magic Program also validates intervals, but the service keeps this guard for persisted/channel inputs.
2. Valid intervals are clamped to at least `config.min_interval` and at most `u32::MAX` milliseconds.
3. If the task ID already exists, only the same authority may replace it; a different authority records a failed scheduling row and leaves the original task intact.
4. `insert_task` writes a monotonic `updated_at` token, replacing any existing row.
5. The service removes any queued old instance, clears retry state, records the new version, and inserts the replacement task with zero delay so it can run immediately.

### Cancel request flow

```text
Magic Program CancelTask instruction
  -> ExecutionTlsStash::register_task(TaskRequest::Cancel)
  -> executor sends over ScheduledTasksTx
  -> TaskSchedulerService::process_cancel_request
  -> remove runtime state and SQLite row when authority matches
```

If the task is missing, runtime queue/retry state is cleaned and the request succeeds. If the authority does not match the persisted task authority, the service logs and returns success without removing the task. This mirrors the service's defensive behavior; signer validation happens in the Magic Program.

### Crank execution flow

1. The main loop waits for `DelayQueue` expirations.
2. When one task expires, it drains all currently expired tasks in the same tick into one batch.
3. The service spawns a Tokio task to send the batch so the main loop can continue receiving schedule/cancel requests and cleanup ticks.
4. `send_crank_batch` reads the latest blockhash, then uses a `JoinSet` to send one transaction per task concurrently.
5. Each transaction includes a Magic Program noop instruction with a unique counter and an `execute_task_instruction(task.authority, task.instructions.clone())` instruction, signed by `validator_authority()` with `validator_authority_id()` as payer.
6. The batch result is sent back over an internal unbounded channel.
7. `on_crank_batch_completed` prepares success/failure DB mutations, applies them through `apply_crank_batch_completion`, then updates the delay queue only for rows whose optimistic `updated_at` token still matches.

A successful first execution anchors `last_execution_millis` at completion time; recurring executions preserve fixed-rate cadence by adding the interval to the previous `last_execution_millis`. Overdue recurring executions are requeued with zero delay.

### Failure, retry, and stale completion flow

- Only `TaskSchedulerError::Rpc(_)` is retryable for crank execution.
- Retryable failures use exponential backoff based on `max(slot_interval, 100ms)`, capped at 5 seconds, for at most 10 retries.
- Non-retryable failures and exhausted retries delete the task from `tasks` and insert a `failed_tasks` row.
- Stale in-flight completions are ignored using both SQLite `updated_at` checks and in-memory `task_versions`. This protects task replacement/cancellation that races with an already spawned crank send.
- On cancellation-token shutdown, the service breaks the select loop, drops the internal sender, and drains completed crank batches still present on the internal receiver before returning.

### Failed-record cleanup flow

The service creates a Tokio interval from `failed_task_cleanup_interval.max(1ms)` with `MissedTickBehavior::Delay`. On each tick it computes `now - failed_task_retention` and deletes older rows from both `failed_scheduling` and `failed_tasks` in a single transaction. Cleanup failures are logged and do not stop the service.

## Important internals and caveats

### SQLite persistence and optimistic concurrency

The task row's `updated_at` is a version token as well as a timestamp. `insert_task` ensures replacement tokens are monotonic even when the system clock does not advance. Batch completion updates/deletes include `WHERE id = ? AND updated_at = ?`; if a row changed during an in-flight crank send, completion maps omit that task and runtime state is left untouched.

Do not replace batch completion with per-task commits without considering throughput and race behavior. The current one-transaction batch is intentional.

### Unbounded channels and concurrent sends

The service uses the processor's unbounded scheduled-task channel and an internal unbounded crank-completion channel. Crank sends inside a batch are parallelized with a `JoinSet` and are currently not explicitly bounded beyond the number of tasks that expire together. Heavy scheduled-task workloads can therefore amplify RPC sends; preserve or improve this behavior carefully and report performance risk when changing it.

### Validator authority and crank signer assumptions

Crank transactions are built with `validator_authority()` and include Magic Program `ExecuteTask` instruction helpers that derive the required crank signer PDA from task authority. The Magic Program verifies validator/crank signer constraints. Do not change signer layout or payer selection in this crate without checking `programs/magicblock/src/schedule_task/process_execute_task.rs` and integration tests such as `test_use_crank_signer.rs`.

### Primary-only execution

`magicblock-api` starts the task scheduler only in primary mode. Replica behavior must remain intentional: replicas should not independently crank scheduled tasks unless the validator lifecycle/coordination model is explicitly changed.

## Important invariants

1. Persisted task rows must remain recoverable across restart unless `task_scheduler.reset` removes the database file.
2. Invalid or completed persisted tasks must be removed on startup, not requeued forever.
3. A task ID may be replaced only by the same authority; unauthorized replacement must not mutate the existing task.
4. Cancel requests must remove a task only when the persisted authority matches the cancel authority.
5. `execution_interval_millis` must remain in the valid Magic Program/service range and must be clamped to `config.min_interval` for runtime scheduling.
6. `updated_at` tokens must be preserved on queued/in-flight `DbTask`s and checked before applying completion state.
7. Stale crank completions must not mutate a replacement or resurrect a cancelled task.
8. Successful recurring tasks must decrement `executions_left`, update `last_execution_millis`, and preserve fixed-rate cadence.
9. Final successful executions, unretryable failures, and exhausted retries must remove the active task row.
10. Retryable RPC failures must use bounded backoff and must not busy-loop the delay queue.
11. Crank transactions must use a fresh/latest blockhash and validator authority signer from the local validator context.
12. Shutdown must drain already completed crank batch results from the internal channel before returning.
13. Changes must avoid unnecessary SQLite transactions, long-held mutexes, unbounded logging, and RPC amplification on the scheduled-task path.

## Common change areas and what to inspect

### Changing schedule/cancel semantics

Start with `magicblock-task-scheduler/src/service.rs` (`process_request`, `process_schedule_request`, `process_cancel_request`) and `src/db.rs` (`insert_task`, `remove_task`, `get_task`). Then inspect `programs/magicblock/src/schedule_task/process_schedule_task.rs`, `process_cancel_task.rs`, and `magicblock-magic-program-api/src/args.rs`.

Validate authority checks, invalid intervals, iterations, task replacement, cancellation races, and failure-record behavior. Integration tests to inspect include `test_schedule_task.rs`, `test_reschedule_task.rs`, `test_cancel_ongoing_task.rs`, `test_schedule_error.rs`, and `test_unauthorized_reschedule.rs`.

### Changing crank transaction construction or send behavior

Start with `send_crank_batch`, `on_crank_batch_completed`, and `programs/magicblock/src/utils/instruction_utils.rs` for `execute_task_instruction`. Inspect Magic Program execute-task validation in `programs/magicblock/src/schedule_task/process_execute_task.rs`.

Check validator authority, crank signer PDA, noop uniqueness, blockhash source, payer, transaction signing, RPC endpoint, retry classification, and concurrency. Run or inspect `test_schedule_magic_cpi_crank.rs`, `test_schedule_task_signed.rs`, and `test_use_crank_signer.rs`.

### Changing persistence, recovery, or schema

Start with `magicblock-task-scheduler/src/db.rs` and `load_persisted_tasks` in `service.rs`. Also inspect `magicblock-api/src/magic_validator.rs` for the database path and `magicblock-config/src/config/scheduler.rs` for reset/retention config.

Schema changes need migration/recovery thought; the current code only creates missing tables and does not run versioned migrations. Preserve bincode compatibility for stored `Vec<Instruction>` or add an explicit migration/compatibility path.

### Changing retry/backoff or cleanup

Inspect constants in `service.rs` (`MAX_TASK_EXECUTION_RETRIES`, `TASK_EXECUTION_RETRY_BASE_DELAY`, `TASK_EXECUTION_RETRY_MAX_DELAY`), `is_retryable_task_execution_error`, `task_execution_retry_delay`, `prepare_crank_failure_outcome`, `apply_crank_failure_outcome`, and the failed-record cleanup select branch.

Check that transient RPC failures do not delete tasks too eagerly, permanent errors do not retry forever, and cleanup cannot stop the scheduler.

### Changing startup/shutdown or mode behavior

Inspect `TaskSchedulerService::start`, `run`, and `magicblock-api/src/magic_validator.rs` around task scheduler initialization and primary-mode gating. Preserve cancellation-token handling and the behavior that scheduler startup failures cause the validator process to exit rather than silently running without task cranking.

## Tests and validation

For documentation-only changes, verify paths and cross-references:

```bash
test -f agents/crates/magicblock-task-scheduler.md
grep -n "magicblock-task-scheduler.md" agents/04_crate-map.md AGENTS.md
```

For code changes in this crate, run targeted unit tests first:

```bash
cargo fmt
cargo nextest run -p magicblock-task-scheduler
```

For config changes, also run:

```bash
cargo nextest run -p magicblock-config task_scheduler
```

For runtime behavior changes, run the integration suite that starts validators:

```bash
cd test-integration
make test-task-scheduler
```

For isolated debugging, use the workflow in `agents/05_testing-and-validation.md`:

```bash
cd test-integration
make setup-task-scheduler-devnet
# in another terminal, run a focused cargo nextest command in test-task-scheduler
```

Before handing off Rust changes, run the broader baseline when practical:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance-sensitive changes should report whether concurrency, SQLite transaction count, lock hold time, RPC send volume, or scheduler startup/recovery latency was measured or only reasoned about.

## Related docs

- `AGENTS.md` for repository-wide agent rules and the requirement to keep `agents/` current.
- `agents/02_specification.md` for Magic Program scheduled tasks in the broader validator model and startup/shutdown expectations.
- `agents/03_architecture.md` for background service boundaries and validator startup flow.
- `agents/04_crate-map.md` for crate ownership and dependency discovery.
- `agents/05_testing-and-validation.md` for task scheduler integration commands and validation reporting.
- `agents/crates/magicblock-api.md` for validator orchestration/startup responsibilities.
- `agents/crates/magicblock-config.md` for config loading and env/TOML behavior.
- `agents/crates/magicblock-core.md` for shared channels and `ExecutionTlsStash`/link responsibilities.
- `agents/crates/magicblock-magic-program-api.md` for task request and Magic Program instruction wire types.
- `magicblock-task-scheduler/README.md` for operator-facing scheduler configuration and performance notes.
- `test-integration/test-task-scheduler/` for end-to-end scheduled-task behavior.
