# `magicblock-table-mania`

## Purpose

`magicblock-table-mania` manages Solana Address Lookup Tables (ALTs) for MagicBlock's base-layer settlement pipeline. The committor service uses it while preparing commit, undelegation, finalize, action, and buffer-delivery transactions that would otherwise exceed transaction account limits.

High-level responsibilities:

- create lookup tables with validator-derived table authorities;
- extend active tables with pubkeys needed by pending base-layer transactions;
- track local reference counts so shared tables are kept alive until all requesters release their pubkeys;
- fetch finalized `AddressLookupTableAccount` values after local create/extend transactions have landed and finalized remotely;
- deactivate and close released tables through an optional background garbage collector;
- expose low-level table helpers for integration tests and table-discovery tooling.

This crate is on the base-layer settlement preparation path and is performance-sensitive. Changes must avoid unnecessary RPC amplification, long-held async locks, duplicate table creation, or extra finalization waits that slow commit delivery.

End-to-end commit/undelegation semantics live in .agents/specs/validator-specification.md; this crate owns ALT reservation, finalized lookup-table readiness, release, and garbage-collection support for settlement transactions.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-table-mania` change. In particular, update it for changes to:

- public exports in `src/lib.rs`, `TableMania`, `LookupTableRc`, `GarbageCollectorConfig`, `TableManiaComputeBudgets`, or `TableManiaError`;
- reservation/release semantics, reference-count behavior, or active/released table lifecycle;
- create/extend/deactivate/close instruction layout, signer requirements, authority derivation, compute budgets, priority fees, or send-confirmation policy;
- local-to-remote readiness waits in `try_get_active_address_lookup_table_accounts`;
- retry/fallback behavior for failed table extension, chain reconciliation, or close salvage;
- `randomize_lookup_table_slot` feature or `RANDOMIZE_LOOKUP_TABLE_SLOT` environment behavior;
- metrics emitted through `magicblock-metrics`;
- committor call-site expectations or integration-test workflows for table preparation.


For the general documentation-update rule, see .agents/memory/agent-memory-and-docs.md.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-table-mania/Cargo.toml` | Package metadata, dependencies on `magicblock-rpc-client` and `magicblock-metrics`, and the `randomize_lookup_table_slot` feature used by parallel integration tests. |
| `magicblock-table-mania/src/lib.rs` | Public crate surface. Re-exports compute-budget types/constants, `find_open_tables`, `LookupTableRc`, `MAX_ENTRIES_AS_PART_OF_EXTEND`, and all manager exports. |
| `magicblock-table-mania/src/manager.rs` | High-level `TableMania` manager, reservation/release flows, local/remote readiness waits, extension fallback/retry policy, and optional garbage collector. |
| `magicblock-table-mania/src/lookup_table_rc.rs` | Ref-counted lookup-table representation, deterministic derived authority, create/extend/deactivate/close transactions, chain reconciliation, and low-level table reads. |
| `magicblock-table-mania/src/compute_budget.rs` | Compute-unit limits and priority fees used for table init, extend, deactivate, and close transactions. |
| `magicblock-table-mania/src/derive_keypair.rs` | Deterministic derived table-authority keypair generation from the validator authority, slot, and sub-slot. |
| `magicblock-table-mania/src/find_tables.rs` | Helper for finding table accounts by recomputing derived authorities and table addresses over a slot/sub-slot range. |
| `magicblock-table-mania/src/error.rs` | `TableManiaError`, `TableManiaResult`, signature extraction, and invalid-instruction-data classification used by extension fallback. |
| `magicblock-committor-service/src/committor_processor.rs` | Constructs one `TableMania` with `GarbageCollectorConfig::default()` for the committor processor. |
| `magicblock-committor-service/src/transaction_preparator/delivery_preparator.rs` | Main runtime consumer. Reserves lookup-table pubkeys, fetches finalized ALT accounts, and releases pubkeys during cleanup. |
| `test-integration/test-table-mania/` | Integration tests for create/extend/deactivate/close, reserve, release, ensure, and table-discovery behavior. |
| `magicblock-metrics/src/metrics/mod.rs` | Defines metrics emitted by TableMania local instrumentation. |

Main consumers:

- `magicblock-committor-service`, especially `DeliveryPreparator`, for ALT preparation before base-layer transaction delivery;
- `magicblock-account-cloner`, indirectly through committor errors that may carry a `TableManiaError` signature for diagnostics;
- `test-integration/test-table-mania` and committor integration tests.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exports:

- `pub mod error`;
- compute-budget constants and types from `compute_budget.rs`;
- `find_open_tables` and `FindOpenTablesOutcome`;
- `LookupTableRc` and `MAX_ENTRIES_AS_PART_OF_EXTEND`;
- manager exports including `TableMania` and `GarbageCollectorConfig`.

### `TableMania`

`TableMania` is cheap to clone because it wraps shared state in `Arc` locks. It stores:

- `active_tables: Arc<RwLock<Vec<LookupTableRc>>>` for tables that may satisfy reservations;
- `released_tables: Arc<Mutex<Vec<LookupTableRc>>>` for tables with no remaining reservations and pending GC;
- one authority pubkey, a `MagicblockRpcClient`, the table-slot randomization setting, and default compute budgets.

Important methods:

- `new(rpc_client, authority, garbage_collector_config)` creates the manager and optionally spawns a non-terminating Tokio GC task;
- `reserve_pubkeys(authority, pubkeys)` increments refs for existing keys and creates/extends tables for missing keys;
- `ensure_pubkeys_table(authority, pubkeys)` ensures keys are present, but does not increase refs for keys already present;
- `try_get_active_address_lookup_table_accounts(pubkeys, wait_for_local_table_match, wait_for_remote_table_match)` returns finalized `AddressLookupTableAccount`s for reserved/present keys;
- `release_pubkeys(pubkeys)` decrements refs and moves fully unreserved tables to `released_tables`;
- inspection helpers such as `active_tables_count`, `released_tables_count`, `active_table_addresses`, `released_table_addresses`, `active_table_pubkeys`, and `get_pubkey_refcount`.

The authority passed to `reserve_pubkeys` and `ensure_pubkeys_table` must match the authority used to construct the `TableMania`; mismatches return `TableManiaError::InvalidAuthority`.

### `LookupTableRc`

`LookupTableRc` is the low-level table state enum:

- `Active` stores the derived table authority keypair, table address, ref-counted pubkeys, creation slot/sub-slot, init signature, extend signatures, and an `extendable` flag;
- `Deactivated` stores the derived authority, table address, deactivation slot, and deactivate signature.

Important methods include `init`, `extend`, `extend_respecting_capacity`, `reconcile_with_chain`, `deactivate`, `is_deactivated_on_chain`, `close`, `is_closed`, `get_meta`, `get_chain_pubkeys`, and `get_chain_pubkeys_for`.

`MAX_ENTRIES_AS_PART_OF_EXTEND` is currently `24`, based on transaction-size constraints with compute-budget instructions. Do not increase it without validating transaction fit and compute behavior.

### Compute budgets and errors

`TableManiaComputeBudgets::default()` supplies per-operation budgets for init, extend, deactivate, and close. Each table transaction prepends both `set_compute_unit_limit` and `set_compute_unit_price` instructions. `TableManiaError` wraps `MagicBlockRpcClientError` and preserves signatures where possible for diagnostic log/CU lookups.

## Runtime flows

### Committor ALT preparation flow

```text
DeliveryPreparator::prepare_for_delivery
  -> prepare_lookup_tables
  -> TableMania::reserve_pubkeys
  -> TableMania::try_get_active_address_lookup_table_accounts
  -> VersionedMessage compilation uses returned AddressLookupTableAccount values
  -> DeliveryPreparator::cleanup releases lookup-table pubkeys
```

`DeliveryPreparator` currently waits up to 50 seconds for local table matches and up to 50 seconds for finalized remote table matches. The remote wait is intentional: lookup table create/extend transactions must be finalized before the resulting ALT accounts are usable in base-layer transactions.

### Reservation and table creation/extension

1. `reserve_pubkeys` first tries to reserve keys already present in active tables.
2. Missing keys go through `reserve_new_pubkeys` under manager-level active-table locks.
3. The manager tries to extend the last active, not-full table before creating a new table.
4. Each create/extend writes at most `MAX_ENTRIES_AS_PART_OF_EXTEND` keys per transaction.
5. Successful low-level table operations insert local keys and set initial refcounts.
6. If an existing table extension fails, the manager reconciles local state with chain and either retries, marks the table non-extendable and creates a new table for invalid-instruction-data failures, or returns the error after the retry budget is exhausted.

Locking is part of correctness here. The write lock on `active_tables` prevents concurrent tasks from creating/extending the same table in conflicting ways. Avoid awaiting under locks unless the lock is deliberately serializing table mutation.

### Fetching finalized lookup table accounts

1. `try_get_active_address_lookup_table_accounts` waits until every requested key is present in local active tables.
2. For each matching table, it records the latest local update send time for the matched keys.
3. Before the first finalized remote fetch, it delays based on an estimated finalization depth: 32 slots at 400 ms plus a 200 ms buffer.
4. It polls `get_multiple_accounts_with_commitment(..., CommitmentConfig::finalized(), ...)` until every matched table exists remotely and contains every locally matched key.
5. It returns `AddressLookupTableAccount` values populated from the finalized remote table contents.

Do not relax finalized commitment in this flow unless the consuming transaction path is explicitly changed and tested; non-finalized ALT updates may not be usable by later base-layer transactions.

### Release and garbage collection

1. `release_pubkeys` decrements local refcounts for all released keys.
2. While holding the active-table write lock, it drains active tables and moves tables with no remaining reservations to `released_tables`.
3. If configured, the background GC periodically calls `deactivate_tables` and `close_tables`.
4. Deactivation sends a table-deactivate transaction and converts the `LookupTableRc` to `Deactivated` with the current slot.
5. Closing first checks the Solana deactivation window using `MAX_ENTRIES` slot hashes, then sends a close transaction and verifies the account no longer exists.
6. Close errors are retried on later GC cycles; if a close error indicates a prior close may have landed, `is_closed` salvages the state so the table can be removed from `released_tables`.

The GC task logs errors instead of bubbling them because there is no request context to fail. Future edits must preserve retry-on-next-cycle behavior or replace it with an explicit operational recovery path.

## Important internals and caveats

### Derived authorities and table addresses

Table authority keypairs are deterministically derived from the validator authority, slot, and sub-slot by signing the seed material and hashing the signature. This lets `find_open_tables` recompute candidate table addresses later. `create_new_table_and_extend` increments a process-wide `SUB_SLOT` when multiple tables are created in the same slot.

The `randomize_lookup_table_slot` feature, or `RANDOMIZE_LOOKUP_TABLE_SLOT` without the feature, randomizes the sub-slot source to avoid table-address collisions in parallel tests. Integration test crates enable the feature.

### Local state may be ahead of finalized chain state

A table create/extend can be locally recorded immediately after `send_transaction` returns successfully, but the finalized table account may lag. `latest_update_sent_at` and the remote-readiness wait are designed to avoid fetching too early. Removing this delay can cause transaction compilation/delivery failures that are difficult to reproduce.

### Refcounts and `ensure_pubkeys_table`

`reserve_pubkeys` represents a checkout and increments refs for existing keys. `ensure_pubkeys_table` is intended for existence checks: it does not increase refs for already-present keys, but newly inserted keys are created through the same low-level path and therefore start with a refcount of 1 in the current implementation. Preserve the tested behavior unless changing the public contract and tests together.

### Metrics

This crate increments local instrumentation around finalized remote-read readiness and close verification. Keep this guide focused on local instrumentation intent; metric naming, labels, and registry details belong in `.agents/context/crates/magicblock-metrics.md`.

## Important invariants

1. A `TableMania` instance must use exactly one authority. Mutating methods must reject a different authority.
2. Lookup table create/extend/deactivate/close transactions must be signed by the validator authority and the deterministic derived table authority where required by the ALT program.
3. No create or extend transaction may include more than `MAX_ENTRIES_AS_PART_OF_EXTEND` pubkeys.
4. A deactivated table must never be extended.
5. Refcounts must not underflow; releasing an unreserved key must be a no-op.
6. Tables may move from active to released only when all contained pubkeys have zero reservations.
7. Returned `AddressLookupTableAccount`s must reflect finalized remote table contents containing all requested keys.
8. Extension failure handling must preserve local/remote consistency by reconciling chain state before retrying or falling back to a new table.
9. GC must tolerate transient RPC or chain errors and retry deactivate/close work later.
10. Changes must avoid unnecessary RPC calls and lock contention on the settlement preparation path.

## Common change areas and what to inspect

### Changing reservation, release, or concurrency behavior

Start with `magicblock-table-mania/src/manager.rs` (`reserve_pubkeys`, `reserve_new_pubkeys`, `extend_table`, `release_pubkeys`) and `src/lookup_table_rc.rs` (`RefcountedPubkeys`, `reserve_pubkey`, `release_pubkey`, `has_reservations`). Then inspect `test-integration/test-table-mania/tests/ix_reserve_pubkeys.rs`, `ix_release_pubkeys.rs`, and `ix_ensure_pubkey_table.rs`.

Check for duplicate table creation, missing releases, refcount changes for overlapping requests, and awaits while holding locks.

### Changing ALT transaction construction

Inspect `src/lookup_table_rc.rs` methods `init`, `extend`, `deactivate`, and `close`, plus `src/compute_budget.rs`. Also inspect `magicblock-rpc-client` send-confirmation behavior because `LookupTableRc::get_send_transaction_config` uses processed confirmation for processed RPC clients and committed confirmation for confirmed/finalized clients.

Validate transaction fit, signer lists, instruction indexes, compute unit limits, priority fees, and error classification. The invalid-instruction-data fallback assumes the extend instruction is at index `2` in existing-table extend transactions.

### Changing remote readiness or timeouts

Inspect `try_get_active_address_lookup_table_accounts`, `remote_table_finalization_delay`, and `DeliveryPreparator::prepare_lookup_tables`. Preserve finalized commitment unless intentionally changing the base-layer transaction delivery contract.

### Changing table cleanup

Inspect `release_pubkeys`, `launch_garbage_collector`, `deactivate_tables`, `close_tables`, and `LookupTableRc::close`. If changing close behavior, run or reason about the long `test_table_close` feature path; table deactivation can take minutes.

### Changing derived-address behavior or table discovery

Inspect `src/derive_keypair.rs`, `LookupTableRc::derive_keypair`, `create_new_table_and_extend`, and `src/find_tables.rs`. Any change affects ability to discover tables by slot/sub-slot and may strand existing tables.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-table-mania`.
- Relevant integration suites: TableMania lifecycle/reservation tests, the long close/deactivation path when close behavior changes, and committor preparator tests for settlement-path changes; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Performance validation intent: report whether settlement-preparation behavior was measured or only reasoned about, especially for reservation, remote readiness, compute budgets, RPC behavior, and lock contention.


## Adjacent implementation references

- See `.agents/context/crates/magicblock-rpc-client.md` for the RPC wrapper used by all table transactions and remote reads.
- Consult `.agents/context/crates/magicblock-committor-service.md` when changing the main runtime ALT preparation and cleanup consumer.
- Inspect `magicblock-committor-service/src/transaction_preparator/delivery_preparator.rs` to understand the main runtime call site.
- Refer to `magicblock-metrics/src/metrics/mod.rs` for local metric definitions emitted by this crate.
- Use `test-integration/test-table-mania/` to review integration coverage of table lifecycle and reservation behavior.
