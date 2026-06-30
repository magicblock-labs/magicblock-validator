# `magicblock-accounts-db`

## Purpose

`magicblock-accounts-db` is the validator's local account storage crate. It backs execution, RPC reads, account synchronization, replication, and tooling with a persistent, memory-mapped account store plus LMDB secondary indexes.

At a high level it:

- stores `AccountSharedData` records in an append-oriented mmap file at `<storage>/accountsdb/main/accounts.db`,
- indexes accounts by pubkey, owner/program, allocation size, and previous owner metadata in LMDB,
- returns borrowed account views directly from mmap for low-allocation hot-path reads,
- supports atomic batch insertion, account removal, owner-index maintenance, program-account scanning, and repeated-read helpers,
- creates/restores/prunes compressed account snapshots used for rollback and replication bootstrap,
- supports startup maintenance such as stale-bank reset and optional best-effort defragmentation.

This crate is on multiple performance-sensitive paths: transaction execution account loads/commits, RPC `getAccountInfo`/program-account reads, account cloning, ledger replay, replication snapshot/reset flows, and startup/shutdown persistence. Changes must preserve low-latency indexed lookups, low-allocation mmap reads, bounded LMDB transactions, and exclusive-access requirements around maintenance operations. Do not add avoidable deserialization, cloning, blocking I/O, heavy logging, or long-held locks to account read/write hot paths.

## Update requirement

Whenever behavior in `magicblock-accounts-db` changes, or another crate changes AccountsDb flows/contracts, update this document in the same change for changes to:

- storage layout, mmap header fields, block sizing, allocation/recycling, or serialized account representation,
- LMDB tables, key/value encodings, owner/program indexes, or cursor/iterator lifetime handling,
- public APIs such as `AccountsDb`, `AccountsReader`, `AccountsScanner`, `AccountsBank`, or `AccountsDbError`,
- snapshot creation, archive validation, external snapshot insertion, rollback, checksum, or pruning behavior,
- startup reset/defragmentation rules and protected account lists,
- config fields consumed from `magicblock-config::config::AccountsDbConfig`,
- safety requirements for borrowed mmap account data, snapshots, checksums, and defragmentation,
- tests or validation commands relevant to this crate,
- performance characteristics of execution/RPC/account-sync/storage hot paths.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

Primary source files:

| Path | Role |
|---|---|
| `magicblock-accounts-db/src/lib.rs` | Main `AccountsDb` facade, public account APIs, batch insert/rollback, snapshots, reset, defragmentation, checksum, `AccountsReader`, and `AccountsScanner`. |
| `src/storage.rs` | Memory-mapped `accounts.db` file, `StorageHeader`, block allocation, account serialization/deserialization, flush/reload, and defragmentation byte moves. |
| `src/index.rs` | LMDB index manager for pubkey lookup, owner/program scans, owner-change reconciliation, deallocation tracking, and account-move application. |
| `src/index/table.rs` | Thin typed wrapper around LMDB database operations. |
| `src/index/iterator.rs` | LMDB cursor/transaction-backed offset/pubkey iterator for all-account and program-account scans. |
| `src/index/utils.rs` | LMDB environment flags and `AccountOffsetFinder` used by repeated-read `AccountsReader`. |
| `src/snapshot.rs` | Snapshot strategy detection, archive creation/registration, external snapshot insertion, rollback extraction, and pruning. |
| `src/reset.rs` | Protected sysvar/native/Magic Program accounts and startup stale-bank reset allowlist. |
| `src/traits.rs` | `AccountsBank` trait used by chainlink, processor, tests, and mock banks. |
| `src/error.rs` | `AccountsDbError` and logging helper trait. |
| `src/tests.rs`, `src/index/tests.rs` | Unit coverage for account storage, owner indexes, snapshots, rollback, defragmentation, checksum, reset, and index allocation behavior. |
| `magicblock-accounts-db/README.md` | Existing crate overview and safety warning for borrowed mmap state. |

Main consumers:

- `magicblock-api` constructs `AccountsDb` during validator startup, may insert external snapshots from replication, optionally defragments on startup, and resets stale accounts before serving work.
- `magicblock-processor` uses `AccountsBank::get_account` for SVM callbacks, writes execution results, and pauses executors before unsafe snapshot creation.
- `magicblock-aperture` serves RPC reads and transaction submission paths from local account state.
- `magicblock-chainlink` and `magicblock-account-cloner` read/write local clones and evict/update accounts through `AccountsBank`.
- `magicblock-replicator` snapshots, resets, and replays state for replica mode.
- `test-kit`, integration tests, and tools such as `tools/ledger-stats` open/read the database for harnesses and inspection.

Important upstream dependencies:

- `magicblock-config::config::AccountsDbConfig` supplies `database_size`, `block_size`, `index_size`, `max_snapshots`, `defragment_on_startup`, and `reset`.
- `solana_account::AccountSharedData` supplies the owned/borrowed account representation and MagicBlock flags (`delegated`, `ephemeral`, `undelegating`, `confined`, owner-changed tracking).
- `magicblock-magic-program-api` and Solana SDK IDs define protected accounts kept during startup reset.

## Public API shape / Main public types and APIs

### `AccountsDb`

`AccountsDb` coordinates three subsystems: `AccountsStorage`, `AccountsDbIndex`, and `SnapshotManager`.

Important constructors and helpers:

- `AccountsDb::new(config, root_dir, max_slot)`: opens or creates `<root_dir>/accountsdb/main`, honors `config.reset`, initializes storage/index/snapshots, and calls `restore_state_if_needed(max_slot)`.
- `AccountsDb::open(directory)`: tooling/test helper using default config.
- `database_directory()`: returns the snapshots directory (the parent of `main`).

Important account APIs:

- `insert_account(pubkey, account)`: upserts one account and commits the LMDB transaction.
- `insert_batch(accounts)`: upserts many accounts and rolls back committed `AccountSharedData` values on failure via `unsafe { rollback() }`.
- `get_account(pubkey)`: provided by `AccountsBank`; returns an `AccountSharedData` that is often borrowed from mmap.
- `remove_account(pubkey)`, `remove_where(predicate)`, `contains_account(pubkey)`, `account_count()`.
- `get_program_accounts(program, filter)`: scans the owner/program index and reads matching accounts without a full-db deserialization pass.
- `account_matches_owners(account, owners)`: checks owner bytes directly through the account buffer.
- `reader()`: creates an `AccountsReader` with one LMDB read transaction/cursor for repeated lookups.
- `iter_all()`: iterates all indexed accounts by LMDB account table order.

Lifecycle and maintenance APIs:

- `slot()` / `set_slot(slot)`: read/update the slot in the mmap header.
- `unsafe take_snapshot(slot) -> checksum`: flushes storage/index, computes checksum, copies/reflinks active state, and spawns background archive registration.
- `restore_state_if_needed(target_slot)`: rolls back to the nearest snapshot when current state is ahead of the target.
- `insert_external_snapshot(slot, archive_bytes)`: registers or fast-forwards from a network-provided snapshot only when current DB slot is `0`.
- `reset_bank(validator_id)`: removes stale non-delegated/non-protected accounts after startup/replay.
- `unsafe defragment()`: compacts live allocations leftward and clears deallocation holes. This is best-effort and not crash-recoverable.
- `unsafe checksum()`: hashes active delegated/ephemeral/undelegating/confined borrowed accounts in key-sorted order.
- `flush()` and `storage_size()`.

### `AccountsBank` trait

`src/traits.rs` defines the narrow bank interface consumed by account sync and execution code:

```rust
pub trait AccountsBank: Send + Sync + 'static {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
    fn remove_account(&self, pubkey: &Pubkey);
    fn remove_where(
        &self,
        predicate: impl FnMut(&Pubkey, &AccountSharedData) -> bool,
    ) -> AccountsDbResult<usize>;
}
```

Keep this trait small. It is implemented by production `AccountsDb` and test stubs; broadening it can force account-sync, processor, and test crates to grow storage-specific knowledge.

### `AccountsReader` and `AccountsScanner`

- `AccountsReader::read(pubkey, reader)` and `contains(pubkey)` reuse one LMDB cursor/transaction for repeated reads. It is marked `Send`/`Sync`; cursor/transaction lifetime and drop-order assumptions in `index/utils.rs` must remain valid.
- `AccountsScanner` is the iterator returned by `get_program_accounts`; it holds an index iterator and reads accounts lazily from storage, applying the provided filter.

### Errors

`AccountsDbError` normalizes IO, LMDB, missing-snapshot, and internal errors. `lmdb::Error::NotFound` maps to `AccountsDbError::NotFound`; callers often rely on missing accounts not being fatal.

## Runtime flows

### Normal account upsert/read flow

1. A consumer calls `insert_account` or `insert_batch` with `AccountSharedData`.
2. `AccountsDb::upsert` opens/reuses one LMDB write transaction.
3. Closed ephemeral accounts (`account.ephemeral() && owner == Pubkey::default()`) are removed from indexes instead of written.
4. Borrowed accounts are committed in place. If the owner changed, `AccountsDbIndex::ensure_correct_owner` repairs owner/program secondary indexes first.
5. Owned accounts are serialized to a recycled hole when possible, otherwise to a new mmap allocation from `AccountsStorage::allocate`.
6. `AccountsDbIndex::upsert_account` writes pubkey -> allocation, owner -> `(offset, pubkey)`, owner metadata, and records old allocations as recyclable holes.
7. Reads use the pubkey index to resolve an offset and `AccountsStorage::read_account` to deserialize a borrowed account from mmap.

Pitfalls:

- Borrowed account data may point directly into mmap. Do not hold borrowed accounts across concurrent writes/maintenance unless account locking or explicit exclusive access makes it safe.
- Owner changes must update both `owners` and `programs` indexes; otherwise `get_program_accounts` and `account_matches_owners` diverge from `get_account`.
- `insert_batch` expects the iterator to be cloneable so it can rollback previously committed accounts on failure.

### Program-account scan flow

1. `get_program_accounts(program, filter)` opens an LMDB read transaction and creates an `OffsetPubkeyIter` over duplicate values in the `programs` table.
2. Each iterator item yields `(offset, pubkey)` encoded in the program index.
3. `AccountsScanner::next` reads the account at that offset and applies the caller filter.

This is an RPC/account-query hot path. Avoid changing it to full-db scans or per-account LMDB transactions unless there is no safer option.

### Snapshot and rollback flow

```text
processor superblock / replication bootstrap
  -> pause execution or otherwise guarantee no concurrent state transitions
  -> unsafe AccountsDb::take_snapshot(slot)
  -> flush mmap + LMDB
  -> unsafe checksum over active ER-state accounts
  -> create snapshot directory using reflink when available, legacy deep copy otherwise
  -> background thread archives snapshot-<slot>.tar.gz and registers it
```

Rollback (`restore_state_if_needed`) chooses the nearest snapshot at or before the target slot, extracts it, atomically swaps it into `main`, prunes invalidated newer snapshots, and reloads storage/index handles.

Pitfalls:

- `take_snapshot` and `checksum` are `unsafe` because callers must guarantee state cannot change while the snapshot/checksum observes storage. `magicblock-processor` pauses executors before snapshotting.
- Snapshot archiving runs in a background thread. Tests wait for `snapshot_exists`; production callers must not assume the archive is registered synchronously after `take_snapshot` returns.
- Archive validation only checks that bytes are a valid gzip tar; it does not prove the contained accounts DB is semantically valid.
- `insert_external_snapshot` fast-forwards only for an uninitialized DB (`current_slot == 0`) and refuses duplicate archive slots.

### Startup reset and defragmentation flow

1. `magicblock-api` initializes ledger, opens `AccountsDb`, replays ledger if needed, then optionally calls `unsafe defragment()` before enabling normal scheduler/replication/tick/task work.
2. Non-replica modes call `reset_bank(validator_id)` after replay to remove stale ordinary accounts while preserving delegated, undelegating, ephemeral, confined, feature-owned, sysvar/native, Magic Program, validator identity, and other protected accounts.
3. Primary mode sends a replication reset message so replicas perform reset in stream order.

Defragmentation updates indexes before moving bytes, then copies live allocations leftward, zeroes the old tail, flushes storage and index, and clears the deallocation table.

Pitfalls:

- Defragmentation is not crash-recoverable; interruption after index updates can make storage inconsistent.
- Defragmentation must not run while any reader/writer or borrowed account reference is live.
- `reset_bank` is a lifecycle operation, not a generic garbage collector. Do not remove protected or lifecycle-marked accounts casually.

## Important internals and caveats

### Memory-mapped storage layout

`accounts.db` starts with a 256-byte `StorageHeader` containing atomic `write_cursor`, `slot`, `block_size`, `capacity_blocks`, and `recycled_count`. Account bytes begin immediately after the header. `block_size` is fixed when the database is created and must be one of `Block128`, `Block256`, or `Block512`.

Allocation uses `AtomicU64::fetch_add` on the write cursor. If capacity is exceeded the cursor is best-effort rolled back with `compare_exchange`; callers must still treat `Database full` as a hard insert failure.

### LMDB indexes

`AccountsDbIndex` owns four LMDB tables:

| Table | Key | Value | Purpose |
|---|---|---|---|
| `accounts-idx` | account pubkey | `(offset, blocks)` | Fast account lookup. |
| `programs-idx` | owner pubkey | duplicate `(offset, pubkey)` | Program-account scans. |
| `deallocations-idx` | block count | duplicate `(offset, blocks)` | Best-fit-ish allocation recycling and split holes. |
| `owners-idx` | account pubkey | owner pubkey | Owner-change cleanup for `programs-idx`. |

The byte packing macro uses unaligned reads/writes for compactness. Any layout change is a persistence compatibility change and must update tests and migration/restore expectations.

### Self-referential cursor wrappers

`OffsetPubkeyIter` and `AccountOffsetFinder` store LMDB transactions and cursors in one struct using `unsafe transmute` plus field-drop-order assumptions. If these structs are edited, preserve the invariant that iter/cursor drops before the transaction it borrows from.

### Snapshot strategy

`SnapshotManager` detects filesystem reflink support once and prefers CoW directory copies. On filesystems without reflink, legacy copy captures active mmap bytes into memory for `accounts.db` to avoid copying stale on-disk bytes. Large active databases can make this path memory- and I/O-heavy; report this risk for snapshot changes.

## Important invariants

1. **Borrowed mmap account safety:** borrowed `AccountSharedData` must not be held across concurrent mutation, reset, snapshot checksum, or defragmentation unless external synchronization guarantees exclusivity.
2. **Index/storage consistency:** every live account in `accounts-idx` must point to a valid storage allocation, have an `owners-idx` entry, and have exactly one matching owner entry in `programs-idx`.
3. **Owner changes must repair secondary indexes:** updates where `account.owner_changed()` is true must call `ensure_correct_owner` before committing borrowed data.
4. **Closed ephemeral accounts are removed:** an ephemeral account with default owner represents closure and must be removed from indexes rather than serialized as live state.
5. **Snapshots/checksums require quiescence:** callers of `take_snapshot` and `checksum` must ensure no concurrent state transitions mutate accountsdb.
6. **Defragmentation requires exclusive access and is best-effort:** do not run it after scheduler/RPC/replication work can hold accounts; do not treat it as an atomic migration.
7. **Startup reset must preserve lifecycle/protected accounts:** delegated, undelegating, ephemeral, confined, feature-owned, sysvar/native, Magic Program, and validator identity accounts must survive reset.
8. **External snapshots do not overwrite initialized databases:** `insert_external_snapshot` must not replace a DB whose slot is already greater than zero.
9. **Hot-path reads must remain indexed and low allocation:** avoid full scans, repeated LMDB transaction setup inside loops, unnecessary owned-account conversion, or serialization in transaction/RPC read paths.
10. **Persistence layout changes are compatibility-sensitive:** storage header fields, account serialization assumptions, snapshot archive shape, and LMDB table encodings affect restart, rollback, replicas, and tools.

## Common change areas and what to inspect

### Account read/write behavior

Start with `src/lib.rs::upsert`, `AccountsBank for AccountsDb`, `src/storage.rs::read_account`, and `src/index.rs::upsert_account`. Inspect processor commit paths and chainlink/cloner callers if semantics change. Verify owner indexes, ephemeral closure, allocation recycling, and rollback behavior.

### Program-account/RPC scan behavior

Start with `get_program_accounts`, `AccountsScanner`, `src/index/iterator.rs`, and `programs-idx` updates. Check `magicblock-aperture` RPC callers and tests that rely on owner filtering. Preserve lazy indexed scans.

### Snapshot, rollback, and replication bootstrap

Start with `src/snapshot.rs`, `AccountsDb::take_snapshot`, `restore_state_if_needed`, `insert_external_snapshot`, and `magicblock-processor/src/scheduler/mod.rs::handle_superblock`. Check `magicblock-api` startup snapshot insertion for replicas. Validate archive registration timing and rollback pruning.

### Startup cleanup and maintenance

Start with `reset_bank`, `src/reset.rs`, `defragment`, and `magicblock-api/src/magic_validator.rs::start`. Preserve protected accounts and lifecycle flags. Any new Magic Program/sysvar/native account that must survive reset belongs in `protected_accounts` and tests.

### Config changes

Start with `magicblock-config/src/config/accounts.rs`, config tests, and `AccountsDb::new`. Changing `database_size`, `block_size`, `index_size`, `max_snapshots`, `defragment_on_startup`, or `reset` behavior affects startup and persistence. Keep TOML/env naming aligned with `serde(rename_all = "kebab-case")` and existing config tests.

### Unsafe/lifetime code

Start with `src/storage.rs`, `src/index/iterator.rs`, `src/index/utils.rs`, `AccountSharedData::{serialize_to_mmap, deserialize_from_mmap}`, `take_snapshot`, `checksum`, and `defragment`. Require narrow reasoning and targeted tests for any unsafe edit.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-accounts-db`.
- Related package checks by change area: `magicblock-config` for config parsing, and `magicblock-processor` for execution commits, snapshots, reset, or replication lifecycle.
- Relevant integration suites: `test-restore-ledger`; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Performance validation intent: for read/write/index changes, reason about allocations, LMDB transaction count, lock/transaction lifetime, and mmap deserialization; for snapshot/defragment changes, report I/O, memory, and startup/shutdown impact.

## Adjacent implementation references

- `magicblock-accounts-db/README.md` — crate overview and mmap borrowed-state warning.
- `magicblock-config/src/config/accounts.rs` — accountsdb config fields.
- `magicblock-api/src/magic_validator.rs` — startup, reset, defragmentation, and snapshot call sites.
- `magicblock-processor/src/scheduler/mod.rs` — scheduler snapshot call site.
