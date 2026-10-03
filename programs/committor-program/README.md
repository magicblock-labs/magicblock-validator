# magicblock-committor-program

Stages large settlement payloads in base-chain buffers so they can be delivered
in chunks. The [committor service](../../committor-service/README.md)
coordinates writing, finalizing, and cleaning up those buffers.

## Using it

Use the [committor interface](../committor-interface/README.md) for instruction
builders, address helpers, and shared buffer layouts. This crate implements
instruction processing and account validation; the `no-entrypoint` feature
omits its on-chain entrypoint when using the processor as a library.

A complete buffer is not a completed settlement. The Delegation Program still
controls authorization and final state application. Instruction and stored
data layouts must remain compatible with their readers.

[Back to workspace](../../README.md)
