# magicblock-magic-program-api

Shared instruction definitions, program IDs, account-address helpers, and
response types for the validator's native Magic programs.
[magicblock-program](../magic-program/README.md) implements them.

## Using it

Use the instruction and address helpers instead of copying account order,
instruction tags, or address seeds into applications. Producers and runtime
consumers must agree on these layouts.

Version 2 uses the workspace's modern Solana types and wincode serialization.
It does not provide the legacy `backward-compat` feature. Legacy SDK consumers
must remain on their existing API dependency until migrated to the V2 SDK.

Constructing an instruction with a signer does not grant authority to sign it.

## Releases

This crate follows semver independently of the validator workspace. Breaking
changes to public Rust APIs, dependency types, feature behavior, instruction
encoding, account layouts, or address derivation require a major version bump.
Backward-compatible additions use a minor bump; compatible fixes use a patch bump.
Review inherited dependency changes as well as crate changes when preparing a
release.

Set the crate version explicitly in its manifest in a reviewed PR. After merging
to master, push an annotated `magic-program-api-vX.Y.Z` tag at the release commit
to publish the crate to crates.io. Only stable versions are supported by this
workflow. Validator releases do not bump or publish this crate.

Crate and validator version numbers need not match. A crate release does not imply
support for every validator runtime; SDK dependency upgrades require their own
compatibility review and release.

[Back to workspace](../../README.md)
