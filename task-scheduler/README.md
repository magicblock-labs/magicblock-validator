# magicblock-task-scheduler

Stores and runs recurring application tasks on the leader. Applications schedule
work through the [native Magic programs](../programs/magic-program/README.md);
the scheduler persists it and submits due transactions to Engine.

## Task behavior

Tasks belong to an authority. Replacing or cancelling one requires that same
authority, and cancellation does not undo work already running.

Intervals are subject to a configured minimum and are not exact wall-clock
guarantees. Persisted tasks resume after restart. Execution and progress recording
are separate operations, so applications should tolerate re-execution after a
crash.

## Crank signer

**Deprecated.** New programs should not check the crank signer. Verify Hydra
crank execution from the instructions sysvar instead, with
`ephemeral_rollups_sdk::crank::hydra::require_hydra_crank` (or the Pinocchio
equivalent). The crank signer is kept for existing tasks and will be removed.

A task's instructions may sign with its authority's crank signer, the PDA
`["crank-executor", authority]` under `Crank11111111111111111111111111111111111111`.
Every task is stored in its Hydra crank as scheduled, including the crank
signer's signer flag. Hydra cannot produce that signature, so the Hydra cranker
runs such a crank through the Crank program's `ExecuteCrank` instead of
replaying it. The Crank program reads the instructions from the crank and signs
for the crank signer only when it directly follows Hydra's `Trigger` of a crank
that the validator created. Tasks that do not use the crank signer are replayed
top-level, unchanged.

The crank signer must stay read-only, and it is the only signer a task may use.

## Configuration

Use the `[task-scheduler]` section of the
[leader configuration](../config.validator.example.toml) for timing and retention settings.
The `reset` option deletes the task database; leave it disabled for normal
restarts.

See [scheduled tasks](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/scheduled-tasks.md)
for detailed authority and recovery constraints.

[Back to workspace](../README.md)
