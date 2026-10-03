# magicblock-program

Native validator programs for settlement requests, recurring tasks, callbacks,
and sponsored ephemeral accounts. These run inside Engine and are separate from
the base-chain Delegation Program.

## Working with the programs

Use the [Magic Program API](../magic-interface/README.md) for
instruction builders, IDs, and response types. The
[shared runtime](../../runtime/README.md) installs the programs, while
leader services handle settlement and
[scheduled tasks](../../task-scheduler/README.md).

Callbacks run separately from the base-chain actions they report. Ephemeral-account
operations manage local account creation, resizing, closing, and sponsorship;
their signer and accounting checks are enforced during execution.

Host integrations must prepare the runtime and wire the leader's services before
accepting application work.

[Back to workspace](../../README.md)
