# `solana-storage-proto`

Protobuf schemas and conversion code for the validator's
[legacy ledger][legacy]. This is retained-history compatibility support, not
Engine's current ledger format.

## Updating schemas

Edit `proto/*.proto` and the corresponding conversion logic in `src/convert.rs`.
The build script generates Rust types into Cargo's output directory; do not edit
generated files. Set `PROTOC` to use an explicit compiler; non-Windows builds otherwise use
`protobuf-src` to supply one.

Field numbers, enum values, and conversions for errors, balances, loaded
addresses, logs, return data, and compute metadata are compatibility boundaries.
Older bincode-backed records have their own stored representations and defaults;
a missing historical field must not be presented as newly recorded data.

Keep schema changes coordinated with both readers and producers. Successful
decoding alone does not validate an untrusted transaction or its execution.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[legacy]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-ledger/README.md
