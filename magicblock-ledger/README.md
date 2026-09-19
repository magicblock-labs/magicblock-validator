# magicblock-ledger-deprecated

Keeps history from earlier validator versions accessible by reading their
RocksDB ledgers. New history is stored by Engine.

[Aperture](../magicblock-aperture/README.md) uses it for history missing from
Engine and merges older signature history when the requested range reaches the
legacy ledger. Engine errors are reported rather than hidden by fallback.

## Compatibility

Changes must keep existing records readable; their schemas and conversions are
in [storage-proto](../storage-proto/README.md). This crate reads old history but
does not write live records or migrate them into Engine.

[Back to workspace](../README.md)
