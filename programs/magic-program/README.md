# magicblock-program

Native validator programs for settlement requests, recurring tasks, callbacks,
and Magic accounts. These run inside Engine and are separate from
the base-chain Delegation Program.

## Working with the programs

Use the [Magic Program API](../magic-interface/README.md) for
instruction builders, IDs, and response types. The
[shared runtime](../../runtime/README.md) installs the programs, while
leader services handle settlement and
[scheduled tasks](../../task-scheduler/README.md).

Callbacks run separately from the base-chain actions they report. Magic account
operations create, resize, and close local accounts. A sponsor pays rent, and
signer and accounting checks are enforced during execution.

Scheduling commits requires writable committed accounts, including during CPI.
The instruction reserves each account's next commit ID, charges any fee, and
queues the intent in the same transaction. A failed transaction rolls all three
back. The serialized intent includes the reserved ID; this changes the WIP
intent/outbox format and does not migrate previously persisted intents.

Scheduling logs `Scheduled intent id: <id>` and
`Outbox for scheduled intent: <address>` for each intent instead
of signing a future notification. See the committor's
[receipt contract](../../committor/README.md#settlement-receipts) for tracking completion.

Host integrations must prepare the runtime and wire the leader's services before
accepting application work.

[Back to workspace](../../README.md)
