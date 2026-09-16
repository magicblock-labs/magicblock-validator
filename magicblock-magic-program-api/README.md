# magicblock-magic-program-api

Shared instruction definitions, program IDs, account-address helpers, and
response types for the validator's native Magic programs.
[magicblock-program](../programs/magicblock/README.md) implements them.

## Using it

Use the instruction and address helpers instead of copying account order,
instruction tags, or address seeds into applications. Producers and runtime
consumers must agree on these layouts.

The default build uses the workspace's Solana types. The `backward-compat`
feature supports Solana 2.x consumers; use this crate's reexports consistently
when working across that boundary.

Constructing an instruction with a signer does not grant authority to sign it.

[Back to workspace](../README.md)
