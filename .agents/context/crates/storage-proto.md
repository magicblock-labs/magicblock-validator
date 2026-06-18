# `storage-proto` / `solana-storage-proto`

## Purpose

`storage-proto/` contains the workspace crate whose package and Rust crate name are `solana-storage-proto` / `solana_storage_proto`. It provides protobuf definitions, generated protobuf Rust modules, and conversion glue for ledger transaction-status storage.

High-level responsibilities:

- compile `storage-proto/proto/*.proto` into `prost` message types during the crate build;
- expose generated modules for confirmed blocks, address-signature indexes, and entry summaries through `solana_storage_proto::convert`;
- convert between generated protobuf messages and Solana transaction-status types such as `TransactionStatusMeta`, `VersionedConfirmedBlock`, `TransactionByAddrInfo`, and `EntrySummary`;
- preserve compatibility with older serialized ledger metadata through `Stored*` helper structs and `default_on_eof` serde defaults;
- provide the protobuf message type used by `magicblock-ledger` for the `transaction_status` RocksDB column.

This crate is on the ledger persistence/read path through `magicblock-ledger`. It is not execution logic, but its wire formats and conversion semantics are persistence- and RPC-history-sensitive. Avoid unnecessary allocations or decode/encode work in conversion paths because ledger writes happen when transactions are recorded and ledger reads feed RPC/history queries.

## Update requirement

Update this guide in the same change whenever `storage-proto` behavior or contracts change. In particular, update it for changes to:

- protobuf files under `storage-proto/proto/`, field numbers, enum values, optionality, or package names;
- build-time code generation in `storage-proto/build.rs`, including generated module names or `tonic_prost_build` options;
- public modules or conversion impls in `storage-proto/src/convert.rs`;
- `Stored*` compatibility structs in `storage-proto/src/lib.rs`, especially defaults used to read older serialized ledger data;
- transaction or instruction error enum mappings, including MagicBlock-specific variants such as `CommitCancelled`;
- how `magicblock-ledger` writes, reads, or migrates protobuf values from RocksDB;
- validation commands for protobuf generation, conversion round-trips, or ledger persistence compatibility.

Also update this file if another crate changes a type or persistence contract consumed here, such as Solana transaction-status APIs used by the conversion impls.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

| Path | Role |
|---|---|
| `storage-proto/Cargo.toml` | Package manifest. The package is named `solana-storage-proto`; the Rust library is `solana_storage_proto`. Depends on Solana transaction/status types, `prost`, `bincode`, `serde`, `bs58`, and uses `tonic-prost-build` plus `protobuf-src` at build time. |
| `storage-proto/README.md` | Short operator/developer note: generated structs come from `proto/*.proto`; edit proto files to update them. |
| `storage-proto/build.rs` | Sets `PROTOC` from `protobuf_src::protoc()` on non-Windows when not already set, registers proto files for rebuild, and compiles them with client generation enabled and server generation disabled. Adds test-only `enum_iterator::Sequence` derives for error enums. |
| `storage-proto/proto/confirmed_block.proto` | Wire schema for confirmed blocks, transactions, transaction status metadata, token balances, return data, rewards, block time/height, and address table lookups. |
| `storage-proto/proto/transaction_by_addr.proto` | Wire schema for per-address transaction history entries and the explicit transaction/instruction error enum mappings. |
| `storage-proto/proto/entries.proto` | Wire schema for compact entry summaries. |
| `storage-proto/src/lib.rs` | Public crate root. Exposes `convert`, stored compatibility structs, and conversions between stored/bincode-compatible forms and Solana status types. |
| `storage-proto/src/convert.rs` | Includes generated protobuf modules from `OUT_DIR` and implements `From` / `TryFrom` conversions between generated messages and Solana transaction-status types. Contains conversion tests. |
| `Cargo.toml` | Workspace dependency and `[patch.crates-io]` entry point this crate uses to replace upstream `solana-storage-proto`. Also pins `prost` with a comment to keep it in sync with codegen. |
| `test-integration/Cargo.toml` | Integration workspace patch for the same local `solana-storage-proto` replacement. |
| `magicblock-ledger/src/database/columns.rs` | Defines `cf::TransactionStatus` as a `ProtobufColumn` whose value type is `solana_storage_proto::convert::generated::TransactionStatusMeta`. |
| `magicblock-ledger/src/database/ledger_column.rs` | Encodes/decodes `ProtobufColumn` values with `prost::Message`; includes fallback helpers for older bincode values. |
| `magicblock-ledger/src/store/api.rs` | Writes `TransactionStatusMeta` through this crate's generated type and converts protobuf values back to Solana status metadata for ledger/RPC reads. |

Main consumers:

- `magicblock-ledger` is the direct workspace consumer and uses the generated `TransactionStatusMeta` as persisted RocksDB data.
- Solana dependencies patched by the workspace may depend on `solana-storage-proto`; the root and integration `[patch.crates-io]` entries force them to this local crate.
- RPC/history consumers indirectly depend on this crate through ledger methods that read transaction status, confirmed transactions, and address-signature history.

Important upstream dependencies:

- `prost` and `tonic-prost-build` define generated message APIs and encode/decode behavior.
- `protobuf-src` supplies a bundled `protoc` on non-Windows; Windows users must provide `PROTOC` manually per `Cargo.toml` comments.
- Solana crates such as `solana-transaction-status`, `solana-transaction-error`, `solana-message`, `solana-transaction`, `solana-pubkey`, `solana-signature`, and `solana-hash` define the canonical runtime/RPC types converted by this crate.

## Public API shape / Main public types and APIs

Public surface from `src/lib.rs`:

- `pub mod convert;` is the main public module.
- `StoredExtendedReward`, `StoredExtendedRewards`, `StoredTokenAmount`, `StoredTransactionTokenBalance`, `StoredTransactionStatusMeta`, and `StoredTransactionReturnData` are serde-compatible helper types for older bincode-era representations.
- `default_on_eof` is private but important: it lets deserialization of older stored data default fields that may not exist in older serialized bytes.
- Conversions preserve older fields and explicitly reject deprecated bincode status serialization when loaded addresses are present, because the old format cannot represent them.

Public surface from `src/convert.rs`:

- `convert::generated` includes `OUT_DIR/solana.storage.confirmed_block.rs` generated from `confirmed_block.proto`.
- `convert::tx_by_addr` includes `OUT_DIR/solana.storage.transaction_by_addr.rs` generated from `transaction_by_addr.proto`.
- `convert::entries` includes `OUT_DIR/solana.storage.entries.rs` generated from `entries.proto`.
- `From` conversions cover Solana-to-protobuf paths for rewards, confirmed blocks, transactions, versioned messages, transaction status metadata, token balances, address table lookups, return data, compiled/inner instructions, transaction-by-address entries, and entry summaries.
- `TryFrom` conversions cover protobuf-to-Solana paths that can fail due to invalid bincode error payloads, invalid signatures/pubkeys/hashes, or unmapped enum values.

Key public/generated types consumed outside the crate:

| Type/module | Use |
|---|---|
| `convert::generated::TransactionStatusMeta` | Persisted value type for `magicblock-ledger`'s `transaction_status` column. |
| `convert::generated::ConfirmedBlock` / `ConfirmedTransaction` | Protobuf representation of block and transaction data. |
| `convert::tx_by_addr::TransactionByAddrInfo` | Protobuf representation for address-signature history entries. |
| `convert::entries::Entry` | Protobuf representation for entry summaries. |

## Runtime flows

### Build-time protobuf generation

1. Cargo runs `storage-proto/build.rs`.
2. If `PROTOC` is unset and the target is not Windows, the build script points `PROTOC` at `protobuf_src::protoc()`.
3. The build script registers `confirmed_block.proto`, `entries.proto`, and `transaction_by_addr.proto` with `cargo:rerun-if-changed`.
4. `tonic_prost_build::configure()` compiles the schemas with clients enabled and servers disabled.
5. Generated Rust files are emitted under `OUT_DIR` and included by `convert::{generated, tx_by_addr, entries}`.

Do not check generated files into the repository unless the build strategy changes. The source of truth is `storage-proto/proto/*.proto` plus `build.rs`.

### Ledger transaction-status write/read path

```text
processor/API records transaction
  -> magicblock-ledger::Ledger::write_transaction_status
  -> solana_storage_proto::convert::generated::TransactionStatusMeta::from(TransactionStatusMeta)
  -> LedgerColumn<cf::TransactionStatus>::put_protobuf
  -> prost encodes bytes into RocksDB

RPC/history read
  -> LedgerColumn<cf::TransactionStatus>::get_protobuf
  -> prost decodes generated::TransactionStatusMeta
  -> TransactionStatusMeta::try_from(generated value)
  -> ledger/RPC returns Solana transaction-status data
```

Pitfalls:

- `magicblock-ledger` stores only status metadata as protobuf in this path. It still stores `VersionedTransaction` bytes with bincode in the `Transaction` column.
- `get_protobuf_or_bincode` exists for fallback-compatible reads, but `Ledger::read_transaction_status` currently uses `get_protobuf`; do not assume all ledger columns can transparently read old formats.
- Conversion failures surface as ledger errors and can break RPC history reads for persisted transactions.

### Transaction-by-address and error conversion

1. `TransactionByAddrInfo` converts to `tx_by_addr::TransactionByAddrInfo` by serializing signatures as bytes and optional fields as protobuf messages.
2. `TransactionError` maps to explicit `TransactionErrorType`/`InstructionErrorType` enum values.
3. Some transaction errors require extra `TransactionDetails` or `InstructionError` payloads, for example duplicate instruction indexes, rent account indexes, restricted program account indexes, custom instruction errors, and instruction error indexes.
4. Reverse conversion validates enum values and returns `Err(&'static str)` for unmapped/invalid variants.

The numeric enum values in `transaction_by_addr.proto` are compatibility-sensitive. If Solana or MagicBlock adds/removes transaction errors, update the proto enum, both conversion directions, and the `test_error_enums` coverage together.

## Important internals and caveats

### Protobuf schema compatibility

Treat proto field numbers and enum discriminants as persisted wire format. New fields should generally use new field numbers and optional/repeated fields where old data must remain readable. Renaming a Rust field generated by `prost` is less important than changing a number or type, but generated names still affect conversion code.

### `Stored*` compatibility helpers

`StoredTransactionStatusMeta` and related `Stored*` structs model older bincode-compatible representations. Several fields use `#[serde(deserialize_with = "default_on_eof")]` so older serialized values can still be read after fields were added. Preserve these defaults when adding stored fields unless you have a deliberate migration plan.

### Loaded addresses and deprecated bincode status metadata

`TryFrom<TransactionStatusMeta> for StoredTransactionStatusMeta` rejects values with non-empty `loaded_addresses` because the deprecated bincode format cannot represent them. The protobuf path in `convert::generated::TransactionStatusMeta` does include loaded writable/read-only addresses. Do not route modern v0 transaction metadata through the deprecated stored/bincode conversion unless loaded addresses are impossible.

### Panics versus fallible conversion

Most protobuf-to-Solana conversions are fallible where malformed external-sized data is expected, but some conversions use `expect`/`unwrap` for fields that should be produced only by this crate's own encoding path, such as required message headers or pubkey/hash byte lengths. If you make these types ingest untrusted or externally produced protobuf bytes, consider whether those conversions need to become fallible and update consumers/tests accordingly.

### Workspace patching

The root workspace patches `solana-storage-proto` to `./storage-proto` because Solana dependencies may otherwise pull an upstream crate version with incompatible protobuf tooling. Keep the root and `test-integration` patches aligned when dependency or build-tool versions change.

## Important invariants

1. The crate directory is `storage-proto/`, but the package/crate contract is `solana-storage-proto` / `solana_storage_proto`; preserve this naming unless all workspace patches and consumers are migrated together.
2. Proto field numbers and enum numeric values must remain backward-compatible with persisted ledger bytes.
3. `prost` versions, `tonic-prost-build` output, and workspace comments about `solana-storage-proto` codegen must stay in sync.
4. `TransactionStatusMeta` conversion must preserve status, fee, balances, inner instructions, logs, token balances, rewards, loaded addresses, return data, compute units, and cost units semantics expected by Solana RPC/history consumers.
5. Transaction and instruction error mappings must be exhaustive for the generated enums covered by tests and must preserve MagicBlock-specific `TransactionError::CommitCancelled`.
6. Older stored data must remain readable where compatibility helpers exist; added serde fields should default safely for missing older data.
7. Ledger write/read paths must avoid extra encode/decode cycles, excessive allocation, and heavy logging in hot transaction-history paths.
8. Generated protobuf modules must remain included from `OUT_DIR`; `proto/*.proto` and `build.rs` are the editable sources of truth.

## Common change areas and what to inspect

### Add or change transaction-status fields

Start with:

- `storage-proto/proto/confirmed_block.proto`
- `storage-proto/src/convert.rs` conversions for `TransactionStatusMeta`
- `storage-proto/src/lib.rs` `StoredTransactionStatusMeta` if older bincode compatibility is affected
- `magicblock-ledger/src/store/api.rs` read/write paths and tests around `create_transaction_status_meta`

Check that old data remains readable, RPC-facing metadata still matches Solana expectations, and loaded-address behavior is not regressed.

### Update transaction or instruction error support

Start with:

- `storage-proto/proto/transaction_by_addr.proto`
- `impl TryFrom<tx_by_addr::TransactionError> for TransactionError`
- `impl From<TransactionError> for tx_by_addr::TransactionError`
- `test_transaction_error_encode` and `test_error_enums` in `storage-proto/src/convert.rs`

Preserve numeric enum values. Add new enum values at the end unless a deliberate migration requires otherwise.

### Change protobuf generation or build dependencies

Start with:

- `storage-proto/build.rs`
- `storage-proto/Cargo.toml`
- root `Cargo.toml` workspace `prost`, `protobuf-src`, and `[patch.crates-io]` entries
- `test-integration/Cargo.toml` patches

Validate on a clean build if possible so stale `OUT_DIR` artifacts do not hide codegen problems.

### Change ledger usage of generated types

Start with:

- `magicblock-ledger/src/database/columns.rs` `ProtobufColumn` implementation for `TransactionStatus`
- `magicblock-ledger/src/database/ledger_column.rs` protobuf encode/decode helpers
- `magicblock-ledger/src/store/api.rs` transaction status read/write functions

This can affect persistence compatibility and RPC history. Run both storage-proto conversion tests and ledger tests.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `solana-storage-proto`.
- Codegen/schema changes should include a clean targeted build/test for `solana-storage-proto` when practical.
- Ledger persistence or consumer changes should also include focused `magicblock-ledger` coverage.
- Performance-sensitive paths touched: ledger status writes and history reads. If conversion logic, allocation patterns, or ledger encode/decode behavior changes, report whether targeted ledger tests were run and whether performance validation was skipped.

## Adjacent implementation references

- `storage-proto/README.md` — short protobuf generation note.
- `storage-proto/proto/*.proto` — editable protobuf schemas.
- `storage-proto/build.rs` — protobuf code-generation setup.
- `magicblock-ledger/src/database/columns.rs`, `magicblock-ledger/src/database/ledger_column.rs`, and `magicblock-ledger/src/store/api.rs` — direct ledger consumers.
- `Cargo.toml` and `test-integration/Cargo.toml` — workspace patch points for the local `solana-storage-proto` crate.
