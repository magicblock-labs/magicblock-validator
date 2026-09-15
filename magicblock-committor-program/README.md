# `magicblock-committor-program`

Base-chain buffer program and shared layouts used to deliver large settlement
payloads. It stages bytes for the settlement pipeline; it does not replace the
Delegation Program's authority checks or final state application.

## Buffer lifecycle

The instruction builders construct four operations:

1. `Init` creates buffer and chunk-tracking PDAs for an account and commit ID.
2. `ReallocBuffer` grows a buffer when the payload requires it.
3. `Write` writes a chunk and records its progress.
4. `Close` reclaims the buffer and tracking accounts after use, returning
   their lamports to the validator authority.

Use the provided builders and PDA helpers so account order, seeds, bumps,
offsets, and chunk layouts remain consistent with the processor.

## Library and program use

The crate builds as both a library and a deployable program. Host consumers
enable `no-entrypoint` to use builders and shared changeset types without
exporting the Solana program entrypoint.

Buffer completion is not proof of settlement completion. The
[committor service][committor] owns delivery sequencing and the downstream
commit/finalization transaction. Treat serialized instruction and changeset
layouts as compatibility interfaces.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[committor]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-committor-service/README.md
