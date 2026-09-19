# magicblock-task-scheduler

Stores and runs recurring application tasks on the leader. Applications schedule
work through the [native Magic programs](../programs/magicblock/README.md);
the scheduler persists it and submits due transactions to Engine.

## Task behavior

Tasks belong to an authority. Replacing or cancelling one requires that same
authority, and cancellation does not undo work already running.

Intervals are subject to a configured minimum and are not exact wall-clock
guarantees. Persisted tasks resume after restart. Execution and progress recording
are separate operations, so applications should tolerate re-execution after a
crash.

## Configuration

Use the `[task-scheduler]` section of the
[leader configuration](../config.example.toml) for timing and retention settings.
The `reset` option deletes the task database; leave it disabled for normal
restarts.

See [scheduled tasks](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/scheduled-tasks.md)
for detailed authority and recovery constraints.

[Back to workspace](../README.md)
