# `magicblock-ledger`

## Purpose

`magicblock-ledger` is the validator's local persistent ledger/history store. It wraps RocksDB column families behind typed ledger columns and provides the historical data used by RPC, replay/recovery, replication, task scheduling, tests, and operator tooling.

High-level responsibilities:

- persist block metadata (`blocktime`, `blockhash`, latest-block cache) and confirmed transaction data;
- index transactions by signature, slot/index, and account address for RPC history queries;
- expose efficient latest-block reads/subscriptions without requiring RocksDB reads on hot paths;
- replay persisted successful transactions during validator startup when AccountsDb lags the ledger;
- truncate old ledger data with range tombstones and RocksDB compaction when the configured ledger size is reached;
- report RocksDB column-family and ledger operation metrics.

This crate sits on storage, RPC-history, startup/recovery, replication, and execution-persistence paths. Changes can affect RPC latency, ledger replay correctness, duplicate transaction protection, persistent status visibility, disk growth, shutdown flushing, and recovery after restart. Keep RocksDB reads/writes bounded and avoid adding blocking work to transaction execution or RPC hot paths.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-ledger` change. In particular, update it for changes to:

- `Ledger`, `LatestBlock`, `LatestBlockInner`, `SignatureInfosForAddress`, or exported error/API types;
- RocksDB column families, key encodings, serialization formats, protobuf/bincode compatibility, or column options;
- transaction write/read semantics, address-signature pagination, block assembly, status counting, or signature verification;
- ledger replay behavior in `blockstore_processor`, including replay ordering, slot/blockhash handling, or persisted transaction filtering;
- truncation thresholds, cleanup-slot locking, compaction filters, entry counters, flush/shutdown behavior, or cancellation semantics;
- metrics names/labels, RocksDB perf sampling, validation commands, or performance characteristics;
- consumers in RPC, processor, API startup, replication, task scheduling, or test tooling that alter this crate's assumptions.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-ledger/Cargo.toml` | Package metadata and dependencies on `magicblock-core`, `magicblock-metrics`, `solana-storage-proto`, RocksDB, and Solana transaction/status crates. |
| `magicblock-ledger/README.md` | Short overview of the RocksDB-backed ledger, `Ledger`, `Database`, and `LedgerColumn`. |
| `src/lib.rs` | Public crate surface. Re-exports `Ledger`, `SignatureInfosForAddress`, `PerfSample`, and `BLOCKSTORE_DIRECTORY_ROCKS_LEVEL`; defines `LatestBlock` and `LatestBlockInner`. |
| `src/store/api.rs` | Main `Ledger` implementation: open/init, block metadata, transaction/status/memo reads and writes, address-signature queries, perf samples, flush, shutdown, and latest-block access. |
| `src/database/columns.rs` | Column-family names, typed key encodings, deprecated-key compatibility, slot extraction, and column traits. |
| `src/database/db.rs` | `Database` wrapper around RocksDB, typed column construction, batches, range deletes, compaction, storage size, and oldest-slot propagation. |
| `src/database/ledger_column.rs` | Typed column API for bincode/protobuf/raw bytes, iterators, multi-get, RocksDB properties, metrics hooks, and cached entry counters. |
| `src/database/options.rs` | `LedgerOptions`, `LedgerColumnOptions`, storage directory constant, compression options, and currently supported primary access mode. |
| `src/database/rocks_db.rs`, `rocksdb_options.rs`, `cf_descriptors.rs`, `compaction_filter.rs` | Low-level RocksDB open/options/column descriptor setup and purged-slot compaction filtering. |
| `src/blockstore_processor/mod.rs` | Startup replay of persisted blocks/transactions through `TransactionSchedulerHandle`. |
| `src/ledger_truncator.rs` | Background size-based truncation service, range deletion, compaction, cancellation, and truncator lifecycle. |
| `src/metrics.rs` | RocksDB column-family and perf datapoints plus ledger RPC counters. |
| `tests/` and `src/store/api.rs` unit tests | Coverage for block assembly, address-signature pagination, transaction/status reads, counts, truncation, and compatibility behavior. |
| `magicblock-api/src/ledger.rs` | Opens/resets the ledger and manages validator-keypair files relative to the RocksDB directory. |
| `magicblock-api/src/magic_validator.rs` | Starts `LedgerTruncator`, initializes metrics tickers, and replays the ledger on startup when needed. |
| `magicblock-processor/src/executor/processing.rs` | Persists transaction status/metadata into the ledger after execution and broadcasts transaction notifications. |
| `magicblock-aperture/src/requests/http/` | Serves `getTransaction`, `getSignatureStatuses`, and `getSignaturesForAddress` from the ledger. |
| `magicblock-aperture/src/state/` | Seeds RPC blockhash cache from `ledger.latest_block()`. |
| `magicblock-task-scheduler/src/service.rs` | Uses `LatestBlock` as the source of recent blockhashes for scheduled task transactions. |
| `test-kit/src/lib.rs`, `tools/ledger-stats/` | Test harness and manual/operator inspection consumers. |

Main consumers:

- `magicblock-api` for startup, replay, truncator lifecycle, metrics, and shutdown;
- `magicblock-processor` for write-side transaction persistence on the execution path;
- `magicblock-aperture` for RPC-history and blockhash/status reads;
- `magicblock-replicator` and `tools/ledger-stats` for persisted history inspection/replay support;
- `magicblock-task-scheduler`, `magicblock-account-cloner`, and test support through `LatestBlock`.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exposes:

- `Ledger` and `SignatureInfosForAddress` from `store::api`;
- `LatestBlock` and `LatestBlockInner` for lock-free current block metadata;
- `blockstore_processor`, `errors`, and `ledger_truncator` modules;
- `PerfSample` and `BLOCKSTORE_DIRECTORY_ROCKS_LEVEL` for consumers that need perf samples or the RocksDB subdirectory name.

The internal `database` and `metrics` modules are not public. Avoid making lower-level RocksDB internals public unless a consumer truly needs a new boundary; most runtime callers should go through `Ledger` or `LatestBlock`.

### `Ledger`

Important constructors and accessors:

- `Ledger::open(path)` and `Ledger::open_with_options(path, LedgerOptions)` create `path/rocksdb`, adjust `ulimit -n` when enabled, open RocksDB, initialize `LatestBlock` from the highest stored blockhash, and initialize the lowest cleanup slot.
- `ledger_path()`, `banking_trace_path()`, `storage_size()`, `db(self)`, `latest_block()`, and `latest_blockhash()` expose operational handles.
- `flush()`, `sync_wal()`, `shutdown(wait)`, and `cancel_manual_compactions()` are used during shutdown and maintenance.

Primary data APIs:

- Block data: `write_block(LatestBlockInner)`, `get_block(slot)`, `get_max_blockhash()`, `get_lowest_slot()`, blockhash/blocktime counters, and recent perf samples.
- Transactions: `write_transaction(...)`, `read_transaction((signature, slot))`, `get_complete_transaction(signature, highest_confirmed_slot)`, `get_transaction_status(signature, min_slot)`, `read_transaction_status`, `verify_transaction_signature`, and memo read/write helpers.
- Indexes: `get_confirmed_signatures_for_address(pubkey, highest_slot, before, until, limit)`, `read_slot_signature((slot, index))`, `get_highest_transaction_index_for_slot`, and `get_latest_transaction_position`.
- Maintenance: `set_lowest_cleanup_slot`, `delete_range_cf`, `compact_slot_range_cf`, `submit_rocksdb_cf_metrics_for_all_cfs`, and column `HasColumn` access for controlled internal maintenance.

`write_transaction` writes status/indexes before raw transaction bytes. It expects the caller to provide the signature, slot, transaction index, writable/readonly account keys, bincode-serialized `VersionedTransaction`, and `TransactionStatusMeta` produced by execution.

### `LatestBlock`

`LatestBlock` is a cheap, cloneable, single-writer/multi-reader block metadata handle. It stores `LatestBlockInner { slot, blockhash, clock }` in `arc-swap` and broadcasts updates on `store`.

- `load()` is a lock-free read used by RPC, task scheduler, processor, account cloner, and tests.
- `store(block)` atomically swaps the snapshot and notifies subscribers.
- `subscribe()` returns a `tokio::sync::broadcast::Receiver` for block updates.
- It implements `magicblock_core::traits::LatestBlockProvider` for generic consumers.

`LatestBlockInner::new(slot, blockhash, timestamp)` sets `clock.slot` to `slot + 1`, while `LatestBlockInner.slot` remains the block's slot. Preserve that distinction because RPC/sysvar consumers may use the `Clock` value differently from the block height.

### Column and storage model

The ledger uses RocksDB column families with typed key encodings:

| Column | Key | Value | Notes |
|---|---|---|---|
| `TransactionStatus` | `(Signature, Slot)` | protobuf `TransactionStatusMeta` | Key has deprecated-index compatibility. |
| `AddressSignatures` | `(Pubkey, Slot, u32, Signature)` | `AddressSignatureMeta` | Primary index for `getSignaturesForAddress`; not slot-keyed. |
| `SlotSignatures` | `(Slot, u32)` | `Signature` | Allows slot-order iteration and before/until pagination. |
| `Blocktime` | `Slot` | `UnixTimestamp` | Slot column. |
| `Blockhash` | `Slot` | `Hash` | Slot column and source for max/latest block. |
| `Transaction` | `(Signature, Slot)` | bincode `VersionedTransaction` bytes | Same key as `TransactionStatus`. |
| `TransactionMemos` | `(Signature, Slot)` | `String` | Deprecated signature-only key compatibility. |
| `PerfSamples` | `Slot` | bincode `PerfSample` | Slot column. |

When adding or changing a column, update column traits, RocksDB descriptors/options, purge/truncation/compaction handling, counts, tests, and any operator tooling. Key order is part of RPC pagination and compatibility; do not reorder tuple fields casually.

## Runtime flows

### Transaction persistence from execution

```text
processor executor
  -> builds TransactionStatusMeta and account lock lists
  -> Ledger::write_transaction(signature, slot, index, writable, readonly, encoded_tx, meta)
  -> write_transaction_status writes AddressSignatures, SlotSignatures, TransactionStatus
  -> raw bincode VersionedTransaction bytes are written to Transaction
  -> processor broadcasts TransactionStatus to subscribers
```

Preserve the account-key indexing: `AddressSignatures` is what makes account history RPC work, and `SlotSignatures` is what lets pagination locate a before/until signature's transaction index even when that transaction did not include the queried address.

### RPC history reads

`magicblock-aperture` uses the ledger as the persistent fallback/source of truth for historical RPC methods:

1. `getSignatureStatuses` checks the hot transaction cache first, then calls `get_transaction_status(signature, Slot::MAX)`.
2. `getTransaction` calls `get_complete_transaction(signature, u64::MAX)` and encodes the returned Solana transaction/status type.
3. `getSignaturesForAddress` clamps the limit to 1,000, then calls `get_confirmed_signatures_for_address` with optional `before` and `until` signatures.
4. `getBlock`-style reads call `get_block(slot)`, which loads block metadata, reversely iterates slot signatures, and combines transactions with status metadata.

RPC reads hold `lowest_cleanup_slot` read locks while accessing cleanup-sensitive ranges. Do not remove those guards: they prevent cleanup/compaction from racing reads into inconsistent results.

### Latest block and blockhash flow

```text
slot/block producer or replay
  -> Ledger::write_block(LatestBlockInner)
  -> writes Blocktime and Blockhash columns
  -> LatestBlock::store updates lock-free snapshot and broadcasts
  -> RPC BlocksCache / task scheduler / processor / other consumers read latest block cheaply
```

`Ledger::open` seeds `LatestBlock` from `get_max_blockhash()` and the stored block time, defaulting to slot/hash zero when no blocks exist.

### Startup replay

`magicblock-api` calls `blockstore_processor::process_ledger` when the ledger's latest block is newer than AccountsDb. Replay starts at `full_process_starting_slot.saturating_sub(max_age)` so recent blockhashes are restored before replaying transactions. For slots before `full_process_starting_slot`, replay updates only latest block data; from that slot onward, only successful transactions are sanitized without signature verification and replayed through `TransactionSchedulerHandle::replay` with `ReplayPosition { persist: false }`.

Do not persist replayed transactions again from this path, and preserve chronological replay: `get_block` returns transactions newest-first, so replay reverses them to execute in original order.

### Truncation and compaction

`LedgerTruncator` is a background service started by `magicblock-api` with `DEFAULT_TRUNCATION_TIME_INTERVAL` and configured ledger size.

1. A dedicated thread runs a current-thread Tokio runtime and ticks on the truncation interval.
2. It checks `ledger.storage_size()` and skips work until the ledger is near the configured limit.
3. It estimates a safe slot range or handles an overfull ledger with `truncate_fat_ledger`.
4. It advances `lowest_cleanup_slot` / RocksDB `oldest_slot`, inserts range tombstones for slot-keyed columns, marks imprecise counters dirty, flushes tombstones, and compacts affected column families.
5. Compaction filters remove keys whose extracted slot is older than `oldest_slot`.
6. Cancellation is checked before and between manual compactions; `stop()` cancels and `join()` waits for thread exit.

Range deletion is inclusive at the ledger API level even though RocksDB range deletes are end-exclusive. Preserve the existing `to + 1` handling and the special `(to_slot + 1, 0)` upper bound for `SlotSignatures`.

## Important internals and caveats

### Cleanup-slot locking

`check_lowest_cleanup_slot(slot)` rejects reads at or below cleaned slots and returns a read guard that callers must hold across the sensitive read. `ensure_lowest_cleanup_slot()` returns the guard plus the first available slot for iterator bounds. These locks coordinate logical cleanup with RPC/history reads; weakening them can produce inconsistent user-visible history or panics during compaction.

### Serialization compatibility

- Typed columns use bincode through `serde`.
- `TransactionStatus` uses protobuf (`solana-storage-proto`) for stored metadata.
- Some columns implement `ColumnIndexDeprecation` to decode old key layouts; `iter_current_index_filtered` intentionally excludes deprecated keys for current-index iteration.
- `get_protobuf_or_bincode` exists for compatibility paths even though normal status reads use protobuf.

Preserve compatibility for existing ledger directories unless the change explicitly includes a migration/reset plan.

### Counts and metrics

`LedgerColumn` caches entry counts with `DIRTY_COUNT = -1`. Sequential slot columns count via first/last slot; complex columns use RocksDB's estimated key count. Truncation decrements exact sequential counters and marks imprecise counters dirty. If a write/delete path changes a column, update the counter or intentionally mark it dirty.

`maybe_enable_rocksdb_perf` currently returns `None`, so perf-context sampling is disabled even though reporting helpers exist. Column-family metrics are reported through `blockstore_rocksdb_cfs` with low-cardinality labels (`cf_name`, `storage`, `compression`). Do not add high-cardinality labels.

### Open/access modes and directory layout

`Ledger::open(path)` opens the underlying RocksDB database at `path/rocksdb`, matching `BLOCKSTORE_DIRECTORY_ROCKS_LEVEL`. `magicblock-api/src/ledger.rs` reset/lock/keypair helpers depend on this layout. `AccessType` defines primary, primary-maintenance, and secondary variants, but `Rocks::open` currently supports only `Primary`; other variants are unreachable.

### Transaction order conventions

`get_block(slot)` iterates `SlotSignatures` in reverse, so returned block transactions are newest-to-oldest by transaction index. Startup replay reverses them before execution. Tests assert this current API behavior; changing it affects RPC and replay consumers.

## Important invariants

1. **Do not lose committed execution history.** `write_transaction` must keep transaction bytes, status metadata, slot signatures, and address-signature indexes consistent for the same `(signature, slot, index)`.
2. **Keep latest-block reads cheap.** Hot-path consumers must continue using `LatestBlock` instead of RocksDB reads for current blockhash/slot/clock.
3. **Preserve cleanup/read coordination.** Reads over cleanup-sensitive columns must hold the lowest-cleanup read guard or use the provided helpers.
4. **Preserve key ordering.** Column key encodings must maintain the sort order required by reverse slot iteration and account-signature pagination.
5. **Preserve on-disk compatibility.** Deprecated key decoders and protobuf/bincode choices must not be removed without an explicit migration strategy.
6. **Do not replay failed transactions.** Ledger replay must only re-run successful transactions and must use `persist: false`.
7. **Do not block execution/RPC unnecessarily.** Avoid long compactions, full-column scans, excessive serialization, or unbounded iterator work on hot request or transaction paths.
8. **Shutdown must protect durability.** Flush, WAL sync, and RocksDB background-work cancellation behavior must stay compatible with validator shutdown ordering.
9. **Metrics labels must stay bounded.** RocksDB/ledger datapoints should use fixed column/operation labels, not pubkeys, signatures, paths, or other high-cardinality values.

## Common change areas and what to inspect

### Adding or changing historical RPC data

Start with `src/store/api.rs`, `src/database/columns.rs`, and the aperture handler using the data under `magicblock-aperture/src/requests/http/`. Check unit tests in `src/store/api.rs` and `magicblock-ledger/tests/get_block.rs`. Preserve `Slot::MAX` / highest-slot semantics and `getSignaturesForAddress` pagination behavior.

### Changing transaction persistence

Inspect `magicblock-processor/src/executor/processing.rs`, `Ledger::write_transaction`, `write_transaction_status`, `read_transaction`, `get_complete_transaction`, and replay tests in `magicblock-processor/tests/replay.rs`. Ensure the encoded transaction bytes and metadata remain sufficient for RPC encoding and startup replay.

### Changing latest block or blockhash behavior

Inspect `LatestBlock`, `Ledger::write_block`, `Ledger::open`, `magicblock-aperture/src/state/blocks.rs`, `magicblock-task-scheduler/src/service.rs`, and `magicblock-processor` consumers. Confirm that blockhash validity/cache behavior and `Clock` semantics remain compatible.

### Changing replay/recovery

Inspect `src/blockstore_processor/mod.rs` and `magicblock-api/src/magic_validator.rs::maybe_process_ledger`. Validate with ledger restore/replay tests where possible. Preserve replay ordering, blockhash warm-up, no-signature-verify sanitization for restored transactions, and `persist: false`.

### Changing truncation, compaction, or storage size behavior

Inspect `src/ledger_truncator.rs`, `src/database/compaction_filter.rs`, `src/database/columns.rs`, `src/database/db.rs`, and `tests/test_ledger_truncator.rs`. Check entry-counter updates, range bounds, cancellation, flushing before compaction, and whether a column is slot-keyed or only slot-extractable.

### Changing RocksDB options or column families

Inspect `src/database/options.rs`, `src/database/rocksdb_options.rs`, `src/database/cf_descriptors.rs`, `src/database/rocks_db.rs`, `src/database/columns.rs`, and any tooling that opens ledgers. Update reset/path logic if the RocksDB directory name changes.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-ledger`.
- Add focused tests for touched behavior such as address-signature pagination, block assembly, replay ordering, truncation, or serialization compatibility.
- Relevant integration suites: restore-ledger for replay/recovery, `test-pubsub` and `test-magicblock-api` for RPC-visible block/status behavior; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Performance validation is important for execution writes, RPC history reads, startup replay, and truncation/compaction. If no benchmark or load-oriented check is run, report the residual disk-growth, compaction-latency, or resource-use risk.
- Security validation for this crate is persistence correctness and adversarial resource-use behavior; signer/authority checks remain owned by execution/Magic Program layers, with global security policy in `.agents/rules/validator-goals.md` and `.agents/specs/validator-specification.md`.

## Adjacent implementation references

- `magicblock-ledger/README.md` — short crate overview.
- `magicblock-api/src/ledger.rs` and `magicblock-api/src/magic_validator.rs` — startup/reset/replay/truncator integration.
- `magicblock-processor/src/executor/processing.rs` — main write-side call site.
- `magicblock-aperture/src/requests/http/` — RPC-history consumers.
- `test-kit/src/lib.rs` and `tools/ledger-stats/` — test harness and operator inspection consumers.
