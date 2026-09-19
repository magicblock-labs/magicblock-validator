# magicblock-table-mania

Manages base-chain address lookup tables for
[settlement transactions](../magicblock-committor-service/README.md), helping
large transactions fit within message limits.

## Using it

The manager finds or creates tables, adds required addresses, and tracks
reservations. Keep reservations alive while delivery still needs them, then
release them so unused tables can be reclaimed when garbage collection is enabled.

Creating or extending a table sends a base-chain transaction. Wait for the
required readiness before using new addresses; submitting an update alone does
not make a table ready.

[Back to workspace](../README.md)
