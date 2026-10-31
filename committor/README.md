# magicblock-committor-service

Delivers the leader's account commits, undelegations, and associated actions to
the base chain. Start with [`CommittorService`](src/service.rs): it constructs
and supervises intake, execution, and cleanup. The validator starts this service;
scheduling itself runs in Engine on leaders and followers.

## Before and after acceptance

```text
User transaction → admission checks → MagicContext
                                          │
                                    accept transaction
                                          │
                                    durable outbox
                                          │
                         account ordering → execution → report and cleanup
```

- [`intake/`](src/intake): accepting scheduled intents.
- [`execution/`](src/execution): restart recovery, account ordering, task building,
  transaction preparation and sending, confirmation, callbacks, and cleanup.
- [`shared/`](src/shared): account state, task and transaction representation,
  sizing, and access to the durable outbox.

Acceptance moves an intent from MagicContext into its outbox account. Only
accepted intents enter execution. Startup queues recoverable outbox work before
accepting fresh intents. The shared [size estimate](../core/src/intent/limits.rs) cannot calculate the
actual base-account diff, so scheduling is not an execution guarantee.

Large payloads use [commit buffers](../programs/committor-interface/README.md)
and [address lookup tables](../table-mania/README.md). Work affecting the same
accounts is ordered while independent work can proceed concurrently.

Scheduling reserves a commit ID in each committed account and captures it in the
intent. Fees use those IDs, and retries never allocate replacements. A failed
unsettled commit keeps its outbox; dependent work stays blocked for the rest of
the process and can be recovered after restart.

## Completion and shutdown

Local queue acceptance does not mean settlement has completed. Base-chain
actions and local callbacks are separate executions; callback signatures alone
do not confirm their success.

Retry safety depends on whether a transaction could already have taken effect.
Commit-bearing work uses on-chain deduplication, while action-only retries are
restricted to failures before sending. Outbox progress supports recovery, but
does not make the outbox, base chain, and callbacks one atomic operation.

Shutdown stops intake and new attempts, then allows active execution and cleanup
up to three seconds to finish. Remaining work is cancelled; unfinished outbox
records remain available for recovery. Failed or interrupted execution must not
close buffer accounts that a pending transaction might still use.

See the [leader configuration](../config.validator.example.toml) for settings and
[settlement delivery](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/settlement-delivery.md)
for the detailed completion and recovery contract.

[Back to workspace](../README.md)
