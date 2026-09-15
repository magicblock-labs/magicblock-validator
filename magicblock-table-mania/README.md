# `magicblock-table-mania`

Manages base-chain address lookup tables for settlement transactions.
`TableMania` finds or creates tables, extends them with required addresses,
and tracks reservations so active work can retain the tables it needs.

## Table lifecycle

`LookupTableRc` tracks table state and address reference counts. Reserve
addresses for the work that needs them and release them when that work is done.
Optional garbage collection deactivates released tables, then closes them
when the base chain permits it.

Creation and extension are remote transactions. A locally known address is not
necessarily ready for use at the required commitment: readiness accounts for
remote state and recent updates. The manager uses bounded extension retries
and can fall back to a new table for specific existing-table failures.

## Integration

The [committor][committor] uses these tables to fit delivery transactions within
message limits. Compute budgets and RPC confirmation are explicit inputs.
Keep reservations alive through delivery, and do not treat submission of an
extension as proof that a dependent transaction can already use it.

This crate manages base-chain ALTs, not Engine's account cache or transaction
scheduler.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[committor]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-committor-service/README.md
