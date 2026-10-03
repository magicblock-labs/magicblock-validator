# magicblock-committor-interface

Program ID, instruction builders, PDA helpers, errors, and shared settlement
payload and buffer layouts for the [committor program](../committor-program/README.md).

## Using it

Use this crate to construct instructions and read or prepare settlement buffers
without depending on the on-chain processor. The
[committor service](../../committor-service/README.md) uses these types to deliver
large payloads in chunks.

Instruction tags, PDA seeds, and serialized state must remain compatible with
the deployed program and settlement consumers. Constructing an instruction does
not grant authority to execute it.

[Back to workspace](../../README.md)
