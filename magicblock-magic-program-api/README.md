# `magicblock-magic-program-api`

Shared client/runtime interface for the native Magic programs: program IDs,
instruction arguments, PDA derivation, callback responses, and ephemeral-account
constants. This crate defines the interface; [magicblock-program][program]
implements it.

## Building instructions

Use the `instruction`, `args`, and `pda` modules instead of copying instruction
tags, account-index conventions, seeds, or rent constants into consumers.
`response` defines action receipts and callback result envelopes.

Magic, crank, callback, and ephemeral-system programs have distinct IDs and
entrypoints. Supplying an account as a signer in a constructed instruction does
not establish the caller's authority to act as that signer.

## Compatibility

The default build uses the workspace Solana types. The optional
`backward-compat` feature selects compatibility types for Solana 2.x consumers;
use this crate's reexports consistently at that boundary.

Instruction encoding, account order, program IDs, and response layouts must
agree between producers and runtime consumers. Task requests cross the Engine
service-message boundary using wincode. Compatibility aliases do not imply
that arbitrary SDK/runtime version combinations have been validated.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[program]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/programs/magicblock/README.md
