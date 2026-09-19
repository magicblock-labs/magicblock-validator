# solana-storage-proto

Schemas and conversions for the validator's
[legacy ledger](../magicblock-ledger/README.md). These describe retained historical
records, not Engine's current storage format.

## Updating schemas

Edit the [protobuf definitions](proto/) and
[conversion code](src/convert.rs). Rust types are generated during the build;
do not edit generated output. Set `PROTOC` if you need a specific compiler.

Preserve compatibility with existing records, including field numbers and enum
values. Missing fields in old records must not be treated as data that was
actually recorded.

[Back to workspace](../README.md)
