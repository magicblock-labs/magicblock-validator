# magicblock-ledger-deprecated

Reads historical RocksDB ledgers from earlier validator versions. Engine owns
the current ledger; this crate exists only to keep older history accessible.

[Aperture](../magicblock-aperture/README.md) uses it when an Engine history lookup
returns no record. Engine errors are reported rather than hidden by fallback.

## Compatibility

Keep the existing record formats readable. The related
[storage-proto](../storage-proto/README.md) crate provides legacy schemas and
conversions. This is not a live ledger writer or a migration tool.

[Back to workspace](../README.md)
