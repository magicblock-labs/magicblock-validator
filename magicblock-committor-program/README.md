# magicblock-committor-program

Stages large settlement payloads in base-chain buffers so they can be delivered
in chunks. The [committor service](../magicblock-committor-service/README.md)
coordinates writing, finalizing, and cleaning up those buffers.

## Using it

Use this crate's instruction builders and address helpers to create, resize,
write, and close buffers. Host applications enable the `no-entrypoint` feature
to use the library without exporting the on-chain program entrypoint.

A complete buffer is not a completed settlement. The Delegation Program still
controls authorization and final state application. Instruction and stored
data layouts must remain compatible with their readers.

[Back to workspace](../README.md)
