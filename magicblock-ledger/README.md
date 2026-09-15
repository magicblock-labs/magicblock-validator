# `magicblock-ledger-deprecated`

Read-only access to the RocksDB ledger written by earlier validator versions.
The directory retains its historical name, but the Cargo package is explicitly
deprecated. Engine owns current block production and storage.

## Historical RPC fallback

[Aperture][aperture] consults this store after a successful Engine history read
returns no data. Engine errors are not converted into legacy fallback reads.

`Ledger` exposes historical blocks, transactions, signatures, and statuses.
The database/column modules and [storage-proto][storage] preserve their old
encodings. This is compatibility access, not a second live ledger or a migration
writer.

Reads use synchronous RocksDB operations. Aperture bounds concurrent legacy
reads and runs them off its async worker threads; that admission bound does not
guarantee disk latency or bound every queued request.

Retain decoding compatibility while old history remains supported. Do not add
new execution writes here.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[aperture]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-aperture/README.md
[storage]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/storage-proto/README.md
