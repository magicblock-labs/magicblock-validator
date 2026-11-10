# magicblock-magic-program-api

Shared instruction definitions, program IDs, account-address helpers, and
response types for the validator's native Magic programs.
[magicblock-program](../magic-program/README.md) implements them.

## Using it

Use the instruction and address helpers instead of copying account order,
instruction tags, or address seeds into applications. Producers and runtime
consumers must agree on these layouts.

This crate uses the workspace's current Solana types and wincode serialization.
The `backward-compat` feature for Solana 2.x has been removed. Consumers that
still need it must stay on an older API release.

Constructing an instruction with a signer does not grant authority to sign it.

[Back to workspace](../../README.md)
