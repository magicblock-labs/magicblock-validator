# `magicblock-task-scheduler`

Persists and executes repeated application tasks for the leader. This scheduler
is separate from Engine's transaction sequencer and the base-chain committor.

## Request and execution flow

1. Native task instructions emit wincode-encoded `TaskRequest` service messages.
2. `TaskSchedulerService` consumes the Engine message stream and updates SQLite
   plus its in-memory delay queue.
3. Due tasks are submitted directly to Engine as crank transactions.
4. Completion updates or removes the matching stored task version.

The service does not poll TaskContext accounts or submit cranks through
loopback RPC. The crank uses Engine's authority; each task retains its own
authority for application instructions and schedule/cancellation checks.

Replacing a task requires the same authority. Cancellation by a different
authority is ignored. Cancellation does not undo an already-running crank.

## Timing and recovery

New task intervals are clamped to the configured minimum, but first execution
is queued immediately. On restart, persisted tasks wait at least two slot
intervals so a blockhash can become available. An interval is not a precise
wall-clock execution guarantee.

SQLite progress updates are version-conditional: completion of an older task
must not overwrite a replacement's bookkeeping. Engine execution and SQLite
completion are separate operations, however. A crash between them can leave
uncertain completion; recurring instructions should tolerate re-execution.

Retryable failures have bounded in-memory retries. Exhausted or non-retryable
failures move to failure records. Runtime retry counts are reset on reload, so
the bound is not a lifetime limit across restarts. Normal shutdown drains workers
and records completion; error shutdown stops them.

## Configuration

The leader's `[task-scheduler]` section controls reset, minimum interval, and
failure-record retention. `reset` removes the task database; it is not an
ordinary restart option. Retention cleanup removes old failed execution and
scheduling records, not active tasks.

Use the [leader configuration example][leader-config] for field names and
defaults. See [scheduled-task contracts][tasks] for cross-component authority
and recovery requirements.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[leader-config]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/config.example.toml
[tasks]: https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/scheduled-tasks.md
