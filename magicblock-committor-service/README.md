# magicblock-committor-service

Delivers the leader's account commits, undelegations, and associated actions to
the base chain. It prepares settlement transactions, sends them, and handles
confirmation and recovery.

Large payloads use [commit buffers](../magicblock-committor-program/README.md)
and [address lookup tables](../magicblock-table-mania/README.md). Work affecting
the same accounts is ordered while independent work can proceed concurrently.

## Completion and retries

Local execution or queue acceptance does not mean settlement has completed.
Base-chain actions and local callbacks are separate executions; callback
signatures alone do not confirm their success.

Retry safety depends on whether a transaction could already have taken effect.
Commit-bearing work uses on-chain deduplication, while action-only retries are
restricted to failures before sending. Optional persistence supports recovery,
but does not make the database, base chain, and callbacks one atomic operation.

See the [leader configuration](../config.validator.example.toml) for settings and
[settlement delivery](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/settlement-delivery.md)
for the detailed completion and recovery contract.

[Back to workspace](../README.md)
