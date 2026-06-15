# `magicblock-core`

## Purpose

`magicblock-core` is the validator's shared wiring and compatibility crate. It owns the channel types that connect RPC/API dispatch, transaction scheduling, executor outputs, scheduled tasks, and replication; it also provides cross-crate traits, intent payload types, global coordination-mode state, logging helpers, execution thread-local stashing, and shared token/eATA helpers.

This crate is on several performance-sensitive paths:

- transaction submission, simulation, replay, and replication ordering through `link::transactions`;
- account-update and transaction-status fanout through bounded `flume` channels;
- scheduler pause coordination for snapshot/checksum/reset maintenance;
- optimistic RPC/pubsub account reads through `LockedAccount`;
- Magic Program side effects that are collected through `ExecutionTlsStash` during SVM execution.

Keep `magicblock-core` dependency-light and protocol-neutral where possible. It should define shared contracts and small helpers; it must not grow into an owner of RPC policy, account cloning, SVM execution, ledger persistence, committor delivery, or replication service orchestration.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-core` change. In particular, update it for changes to:

- endpoint/channel topology, channel capacity, backpressure semantics, or scheduler pause behavior in `src/link.rs` and `src/link/transactions.rs`;
- public transaction modes, replay/block-boundary ordering, replication message layout, or bincode payload compatibility;
- `LockedAccount` optimistic read behavior or account encoding assumptions;
- `CoordinationMode` states, transitions, or helper semantics used by startup, primary, replica, and Magic Program code;
- `CommittedAccount`, `BaseActionCallback`, `MagicSys`, `LatestBlockProvider`, or `ActionsCallbackScheduler` contracts;
- `ExecutionTlsStash` lifecycle and the set of Magic Program side effects it carries;
- token/eATA derivation, remapping, and projection helpers;
- logging initialization style or feature-gated `tokio-console` behavior;
- validation commands or tests future agents should run for this crate.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-core/src/lib.rs` | Public module surface, `Slot`, `TransactionIndex`, and `debug_panic!`. |
| `magicblock-core/src/link.rs` | Builds paired `DispatchEndpoints` and `ValidatorChannelEndpoints` via `link()`. Defines bounded channel capacity and the shared scheduler pause semaphore. |
| `magicblock-core/src/link/transactions.rs` | Transaction scheduler handle, scheduler commands, processing modes, simulation/replay result types, transaction sanitization wrappers, and scheduler idle guard. |
| `magicblock-core/src/link/accounts.rs` | Account update channel aliases plus `LockedAccount`, the optimistic sequence-lock wrapper used for safe reads of potentially borrowed account data. |
| `magicblock-core/src/link/blocks.rs` | Block hash alias and broadcast receiver type for latest-block notifications. |
| `magicblock-core/src/link/replication.rs` | Serializable replication message envelope and transaction/block/superblock/reset payloads. Variant order and sentinel indices are wire/order compatibility concerns. |
| `magicblock-core/src/coordination_mode.rs` | Process-global atomic coordination mode used by scheduler, RPC, Magic Program, and task/commit scheduling gates. |
| `magicblock-core/src/intent.rs` | Commit/action payload types shared by Magic Program, committor, accounts, and persistence code. Includes ATA-to-eATA remapping for committed accounts. |
| `magicblock-core/src/traits.rs` | Cross-crate trait boundaries for Magic Program syscalls, latest-block access, and action callback scheduling. |
| `magicblock-core/src/tls.rs` | Executor thread-local stash used by Magic Program instructions to emit scheduled task requests for the processor to drain after execution. |
| `magicblock-core/src/token_programs.rs` | Shared SPL Token, Token-2022, ATA, and eATA IDs plus derivation, inspection, remapping, and projection helpers. |
| `magicblock-core/src/logger/` | Tracing initialization, test logger setup, style-specific formatters, and log-rate consolidation helpers. |
| `magicblock-core/Cargo.toml` | Dependency and feature declaration. `tokio-console` enables `console-subscriber` and Tokio tracing. |

Main consumers include:

- `magicblock-api`, which calls `link()` during validator construction and holds the `TransactionSchedulerHandle` for service wiring;
- `magicblock-aperture`, which submits/simulates transactions and consumes account/status events for RPC/pubsub;
- `magicblock-processor`, which consumes `ValidatorChannelEndpoints`, executes `SchedulerCommand`s, drains `ExecutionTlsStash`, and emits account/status/replication messages;
- `magicblock-replicator`, which consumes and publishes `link::replication::Message` and uses `wait_for_idle()` during checksum/reset operations;
- `magicblock-ledger`, which replays persisted transactions through `TransactionSchedulerHandle::replay`;
- `magicblock-accounts`, `magicblock-account-cloner`, and `magicblock-task-scheduler`, which submit validator-internal transactions or consume scheduled tasks;
- `programs/magicblock`, which uses `MagicSys`, `CommittedAccount`, `BaseActionCallback`, coordination mode, and `ExecutionTlsStash`;
- `magicblock-committor-service`, which consumes committed-account/action types and callback scheduling traits;
- `magicblock-chainlink`, which uses token/eATA helpers and consolidated logging helpers.

## Public API shape / Main public types and APIs

### Root exports

- `Slot = u64` and `TransactionIndex = u32` are shared position types used by ledger, processor, RPC, and replication.
- `debug_panic!` panics only in debug builds and logs an error in release builds. Use it only for invariant violations where production should remain alive.

### Channel endpoints

`link::link()` returns the two sides of the validator channel fabric:

```text
DispatchEndpoints (RPC/API side)
  -> transaction_scheduler: TransactionSchedulerHandle
  <- transaction_status: flume Receiver<TransactionStatus>
  <- account_update: flume Receiver<AccountWithSlot>
  <- tasks_service: Option<UnboundedReceiver<TaskRequest>>
  <- replication_messages: Option<mpsc Receiver<Message>>

ValidatorChannelEndpoints (processor/internal side)
  <- transaction_to_process: mpsc Receiver<SchedulerCommand>
  -> transaction_status: flume Sender<TransactionStatus>
  -> account_update: flume Sender<AccountWithSlot>
  -> tasks_service: UnboundedSender<TaskRequest>
  -> replication_messages: mpsc Sender<Message>
  -> pause_permit: Arc<Semaphore>
```

The transaction, account-update, transaction-status, and replication queues use `LINK_CAPACITY = 16384` where backpressure matters. The scheduled-task channel is unbounded because it is drained by the task scheduler service and carries requests produced from execution TLS.

### Transaction scheduling APIs

`link::transactions::TransactionSchedulerHandle` is cloneable and is the public entrypoint for transaction-related dispatch:

- `schedule(txn)` verifies/sanitizes and queues fire-and-forget execution.
- `execute(txn)` verifies/sanitizes, queues execution, and awaits a one-shot `TransactionResult<()>`.
- `simulate(txn)` verifies/sanitizes, queues simulation, and awaits `TransactionSimulationResult`.
- `replay(position, txn)` verifies/sanitizes and queues replay at a specific slot/index with a `persist` flag.
- `replay_block(block)` queues an ordered replicated block boundary and awaits scheduler acknowledgement.
- `wait_for_idle()` acquires the scheduler pause semaphore and returns an `OwnedSemaphorePermit` that keeps scheduling paused while held.

`SanitizeableTransaction` is implemented for `SanitizedTransaction`, `VersionedTransaction`, `Transaction`, and `WithEncoded<T>`. Use `with_encoded(txn)` when an internally constructed transaction also needs bincode bytes for replication or downstream reuse. For unsanitized transaction types, `sanitize(true)` verifies signatures; the `false` path uses a unique hash and is intended only for cases that explicitly skip verification.

### Transaction and replay payloads

Important types in `link::transactions`:

- `SchedulerCommand::{Transaction, Block}` keeps replay transactions and block boundaries in one FIFO command stream so a block cannot overtake preceding transactions.
- `TransactionProcessingMode::{Simulation, Execution, Replay}` controls executor behavior and result notification.
- `ReplayPosition { slot, index, persist }` preserves primary ordering during replication and lets startup ledger replay avoid re-recording/broadcasting.
- `TransactionStatus` contains the committed slot, sanitized transaction, metadata, and slot-local index.
- `SchedulerMode::{Primary, Replica}` is sent to the processor scheduler to switch local scheduling behavior.

### Account event APIs

`link::accounts::LockedAccount` wraps an account update and protects readers from torn reads when `AccountSharedData` is borrowed from memory that another thread can mutate. Use `read_locked` or `ui_encode`; do not bypass the wrapper by holding borrowed account data across asynchronous or long-running work.

### Coordination mode

`coordination_mode::CoordinationMode` is a process-global atomic state:

- `StartingUp`: ledger replay phase; no validator signer and no side effects.
- `Primary`: validator signer and side-effect scheduling are enabled.
- `Replica`: no validator signer and no side effects.

Use `CoordinationMode::current()`, `needs_validator_signer()`, `should_schedule_intents()`, and `needs_onchain_interactions()` for gates. Scheduler mode switches call `switch_to_primary_mode()` or `switch_to_replica_mode()` through processor coordination paths; tests may call them directly but must account for global state leakage.

### Intent/action and trait contracts

- `intent::CommittedAccount` serializes a committed account with `pubkey`, `Account`, and `remote_slot`. `from_account_shared` can override the owner with a parent program ID and remaps delegated ATAs to eATA form when applicable.
- `intent::BaseActionCallback` is the callback payload used for base-action results.
- `traits::MagicSys` lets the Magic Program fetch current commit nonces without depending on the concrete validator service.
- `traits::LatestBlockProvider` abstracts latest slot/blockhash/clock access for services that should not depend on `magicblock-ledger` directly.
- `traits::ActionsCallbackScheduler` abstracts callback transaction construction/scheduling and returns per-callback signatures or construction errors.

### Execution TLS

`tls::ExecutionTlsStash` is a thread-local queue currently used for `TaskRequest`s emitted by Magic Program task scheduling/cancel instructions. The processor clears the stash around execution and drains it after a successful transaction path. Do not use it as a cross-thread channel or persistent store.

### Token/eATA helpers

`token_programs` exports program IDs and helpers for legacy SPL Token, Token-2022, ATA, and eATA handling:

- ATA derivation: `derive_ata`, `derive_ata_with_token_program`, `try_derive_supported_ata_pubkeys`.
- eATA derivation: `derive_eata`, `try_derive_eata_address_and_bump`.
- ATA detection/remapping: `is_ata`, `try_remap_ata_to_eata`.
- eATA projection: `MaybeIntoAta<AccountSharedData>` and `EphemeralAta` conversion/projection helpers.

## Runtime flows

### Validator channel construction

1. `magicblock-api` calls `magicblock_core::link::link()` during validator startup.
2. `link()` creates bounded MPSC queues for transaction commands and replication messages, bounded `flume` queues for account/status events, an unbounded task queue, and a shared `Semaphore(1)` pause permit.
3. The API/RPC side receives `DispatchEndpoints`; the processor side receives `ValidatorChannelEndpoints`.
4. `TransactionSchedulerHandle` clones can be passed to RPC, cloner, accounts, ledger replay, task scheduler, and replication services.

Do not create parallel ad-hoc channels for the same flows without updating this contract and all consumers; ordering and backpressure expectations are centralized here.

### Transaction submit/simulate/replay flow

```text
RPC/service caller
  -> TransactionSchedulerHandle::{schedule,execute,simulate,replay}
  -> SanitizeableTransaction::sanitize_with_encoded(verify = true)
  -> SchedulerCommand::Transaction(ProcessableTransaction)
  -> bounded scheduler command channel
  -> magicblock-processor scheduler/executor
  -> status/account/task/replication outputs
```

`execute` and `simulate` allocate one-shot channels and await processor completion; `schedule` and `replay` only wait for queueing. Choose the lowest-overhead method that preserves caller semantics.

### Replicated transaction and block ordering

1. The primary emits `link::replication::Message` values from the processor scheduler.
2. Replicas receive transaction payloads and block boundaries from `magicblock-replicator`.
3. Transactions are queued through `TransactionSchedulerHandle::replay` with `ReplayPosition`.
4. Block boundaries are queued through `replay_block` as `SchedulerCommand::Block` in the same FIFO scheduler command channel.
5. The block acknowledgement resolves only after the scheduler applies the block and executors acknowledge the slot transition.

Preserve this single ordered command channel. Moving blocks to a separate path can let block boundaries overtake transactions and break replica consistency.

### Scheduler pause / exclusive AccountsDb access

1. External maintenance code calls `TransactionSchedulerHandle::wait_for_idle()`.
2. The future waits until the processor scheduler has released the shared semaphore because all executors are idle and no pending transactions are being processed.
3. The returned `OwnedSemaphorePermit` pauses scheduling while held.
4. Maintenance performs exclusive work such as checksums, snapshots, resets, or defragmentation.
5. Dropping the permit allows scheduling to resume.

Never hold the permit across unrelated I/O or long network operations. It blocks transaction processing and is a critical availability/performance lever.

### Account update read flow

1. Processor emits `AccountWithSlot { account: LockedAccount, slot }` on the account-update channel.
2. RPC/pubsub code receives the update and calls `LockedAccount::read_locked` or `ui_encode`.
3. The first read is optimistic.
4. If the captured `AccountSeqLock` indicates a concurrent write, `LockedAccount` relocks, reinitializes a fresh account view, and retries until it obtains a consistent read.

This flow allows low-overhead reads on the fast path while protecting borrowed account data from concurrent mutation races.

### Magic Program scheduled task flow

1. A Magic Program task instruction runs during SVM execution.
2. The program code calls `ExecutionTlsStash::register_task(TaskRequest::...)`.
3. The processor clears TLS around execution boundaries and, after execution, drains tasks with `next_task()`.
4. Drained tasks are sent over the `tasks_service` channel to `magicblock-task-scheduler`.

The TLS stash is per executor thread. It must be cleared on success, simulation, and failure paths to avoid leaking one transaction's side effects into another transaction on the same worker.

### Commit/action callback flow

1. Magic Program scheduling code constructs `CommittedAccount` and `BaseActionCallback` values.
2. Validator-side adapters implement `MagicSys` to provide commit nonces.
3. Accounts/committor services use `CommittedAccount` payloads to build and persist commit/undelegation work.
4. Committor executors use `ActionsCallbackScheduler` to schedule callback transactions and report `ActionResult`/`ActionError`.

Keep these types serializable and stable enough for persistence and cross-crate use.

## Important internals and caveats

### Channel capacity and backpressure

`LINK_CAPACITY` is intentionally bounded for transaction commands, account/status event channels, and replication messages. Raising it can increase memory and latency under overload; lowering it can cause premature backpressure or dropped service throughput. Scheduled tasks are currently unbounded; if that changes, inspect task scheduler behavior and Magic Program task emission semantics.

### Sanitization and encoded transaction bytes

`with_encoded` bincode-serializes a transaction before wrapping it. If serialization fails it maps to `TransactionError::SanitizeFailure`. Replication relies on `ProcessableTransaction::encoded` to avoid redundant serialization. Do not silently drop encoded bytes on paths that feed replication unless the downstream code has been updated.

### Replication wire compatibility

`link::replication::Message` derives `Serialize`/`Deserialize`; the enum variant order is explicitly part of the wire format. Do not reorder variants or change sentinel indices (`BLOCK_INDEX`, `RESET_INDEX`, `SUPERBLOCK_INDEX`) without a coordinated compatibility plan for primary/replica deployments and persisted/catch-up behavior.

### Coordination mode is global process state

`COORDINATION_MODE` is an atomic static. In tests it starts as `Primary`; otherwise it starts as `StartingUp`. Direct test mutations can leak across tests unless serialized or reset. Runtime switches must keep the processor scheduler's local mode and global coordination mode aligned.

### Token and eATA helpers are protocol-sensitive

`try_remap_ata_to_eata` only remaps delegated token accounts whose pubkey matches the derived ATA for their owner/mint/token program. `EphemeralAta::try_from_account_data` supports both `EPHEMERAL_ATA_LEN` and `LEGACY_EPHEMERAL_ATA_LEN`. Changes here can affect cloning, post-delegation token transfer tests, commit account payloads, and chainlink blacklisting/ATA projection.

### Logging initialization is process-global

`logger::init`/`init_with_config` call `tracing_subscriber::init()` and must be called once at application startup. Tests should use `init_for_tests()`, which uses `try_init()` to tolerate multiple callers. `RUST_LOG_STYLE=EPHEM` or `DEVNET` selects custom formatters; other values use the default formatter.

## Important invariants

1. `link::link()` must return paired endpoints connected to the same channels and pause semaphore; dispatch and validator sides must not be mismatched.
2. Scheduler command ordering must keep replay transactions and replicated block boundaries in the same FIFO stream.
3. `TransactionSchedulerHandle::wait_for_idle()` must continue to pause scheduling while the returned permit is held; maintenance that relies on exclusive `AccountsDb` access depends on this.
4. Bounded channels on hot paths must preserve intentional backpressure and avoid unbounded memory growth.
5. `LockedAccount` readers must use sequence-lock checks for borrowed account data; do not expose APIs that encourage unsafely reading borrowed data after concurrent mutation.
6. `CoordinationMode::Primary` is the only mode that should require validator signing and schedule side effects; `StartingUp` and `Replica` must remain side-effect-free.
7. `ExecutionTlsStash` must be cleared between transaction executions on the same worker thread.
8. Replication `Message` variant order and sentinel indices must remain compatible unless the whole replication protocol is versioned/migrated.
9. Committed-account construction must preserve `remote_slot` and intended owner override semantics, including ATA-to-eATA remapping for delegated token accounts.
10. Token/eATA helper changes must preserve support for legacy SPL Token and Token-2022 derivations where current callers expect both.
11. `magicblock-core` should not depend on heavyweight runtime crates such as API, aperture, processor, ledger, accounts-db, chainlink, or committor service.

## Common change areas and what to inspect

### Changing transaction submission, simulation, or replay

Start with:

- `magicblock-core/src/link/transactions.rs`
- `magicblock-processor/src/scheduler/mod.rs`
- `magicblock-processor/src/executor/processing.rs`
- `magicblock-ledger/src/blockstore_processor/mod.rs` for startup replay
- `magicblock-replicator/src/service/replica.rs` and `magicblock-replicator/src/service/context.rs`
- `magicblock-aperture/src/server/http/dispatch.rs` and transaction request handlers

Check result notification semantics, signature verification, encoded-byte propagation, backpressure, and ordering of `SchedulerCommand::Block` relative to replayed transactions.

### Changing endpoint topology or event channels

Start with:

- `magicblock-core/src/link.rs`
- `magicblock-api/src/magic_validator.rs`
- `magicblock-aperture/src/processor.rs` and subscription state
- `magicblock-processor/src/scheduler/state.rs` and executor output paths
- `magicblock-task-scheduler/src/service.rs`

Ensure every sender/receiver is wired exactly once, optional receivers are moved intentionally, and shutdown behavior remains clear.

### Changing scheduler pause or maintenance coordination

Start with:

- `magicblock-core/src/link/transactions.rs::wait_for_idle`
- `magicblock-core/src/link.rs` pause semaphore creation
- `magicblock-processor/src/scheduler/mod.rs` and `scheduler/coordinator.rs`
- `magicblock-processor/tests/scheduling.rs::test_wait_for_idle_coordination`
- `magicblock-replicator/src/service/context.rs` checksum/reset flows

Verify that exclusive `AccountsDb` operations cannot race executor writes and that the permit is not held longer than necessary.

### Changing account update/read behavior

Start with:

- `magicblock-core/src/link/accounts.rs`
- `magicblock-aperture/src/utils.rs`
- `magicblock-aperture/src/requests/http/mod.rs`
- `magicblock-processor/src/executor/processing.rs`

Preserve fast-path low overhead and slow-path correctness for borrowed account data. Avoid cloning large accounts unless the caller explicitly needs ownership.

### Changing coordination mode or primary/replica behavior

Start with:

- `magicblock-core/src/coordination_mode.rs`
- `magicblock-processor/src/scheduler/coordinator.rs`
- `magicblock-api/src/magic_validator.rs` startup/mode switching
- `programs/magicblock/src/schedule_transactions/process_scheduled_commit_sent.rs`
- `magicblock-aperture/tests/transaction_primary_mode.rs`
- `magicblock-task-scheduler/src/service.rs` tests that call `switch_to_primary_mode()`

Ensure scheduler-local mode, global coordination mode, validator signer requirements, and side-effect gates stay aligned.

### Changing Magic Program side effects or task scheduling

Start with:

- `magicblock-core/src/tls.rs`
- `programs/magicblock/src/schedule_task/`
- `magicblock-processor/src/executor/processing.rs`
- `magicblock-task-scheduler/src/service.rs`

If more side-effect types are added to TLS, document when they are registered, drained, cleared, persisted, and retried.

### Changing commit/action payloads or traits

Start with:

- `magicblock-core/src/intent.rs`
- `magicblock-core/src/traits.rs`
- `programs/magicblock/src/magic_sys.rs`
- `programs/magicblock/src/magic_scheduled_base_intent.rs`
- `magicblock-accounts/src/scheduled_commits_processor.rs`
- `magicblock-committor-service/src/`

Check serialization, persistence, commit nonce behavior, callback error mapping, and base-layer settlement compatibility.

### Changing token/eATA behavior

Start with:

- `magicblock-core/src/token_programs.rs`
- `magicblock-chainlink/src/chainlink/fetch_cloner/ata_projection.rs`
- `magicblock-chainlink/src/chainlink/fetch_cloner/delegation.rs`
- `magicblock-chainlink/src/testing/eatas.rs`
- `programs/magicblock/src/schedule_transactions/process_schedule_commit_tests.rs`
- `test-integration/test-cloning/tests/10_post_delegation_token_transfer.rs`

Verify legacy token and Token-2022 behavior, eATA account length compatibility, rent assumptions, and delegated-account checks.

### Changing logging

Start with:

- `magicblock-core/src/logger/mod.rs`
- `magicblock-core/src/logger/consolidate.rs`
- `magicblock-validator/src/main.rs`
- chainlink/AML tests that call `logger::init_for_tests()`

Avoid adding high-cardinality or noisy logs to hot loops. Keep test logging idempotent.

## Tests and validation

For documentation-only changes to this guide:

```bash
git diff --check -- agents/crates/magicblock-core.md agents/04_crate-map.md AGENTS.md
```

For code changes in `magicblock-core`, run at minimum:

```bash
cargo fmt
cargo test -p magicblock-core
```

Because `magicblock-core` has no dedicated crate-local test suite at the time of writing, also run targeted consumer tests for the subsystem touched:

```bash
# Transaction scheduling, replay, pause semantics
cargo test -p magicblock-processor scheduling replay simulation

# RPC transaction mode/LockedAccount consumers
cargo test -p magicblock-aperture

# Replication message/order or checksum/reset behavior
cargo test -p magicblock-replicator

# Magic Program TLS, commits, callbacks, or token/eATA helpers
cargo test -p magicblock-program
cargo test -p magicblock-chainlink
cargo test -p magicblock-committor-service
cargo test -p magicblock-task-scheduler
```

Then use the workspace baseline from `agents/05_testing-and-validation.md` when time allows:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Integration/manual validation depends on the touched flow:

- transaction/RPC behavior: `cd test-integration && make test-magicblock-api`;
- replication/replay behavior: run the relevant replicator/restore-ledger suites or targeted processor replay tests;
- cloning/token/eATA behavior: `cd test-integration && make test-cloning`;
- scheduled intents/commits/actions: `cd test-integration && make test-schedule-intents` and relevant committor suites;
- task scheduling: `cd test-integration && make test-task-scheduler`.

If a change touches transaction dispatch, account updates, replication, or scheduler pause behavior, report whether performance risk was measured. At minimum, reason about queue backpressure, extra allocations/serialization, lock contention, and whether the change adds work to RPC/scheduler/executor hot paths.

## Related docs

- `AGENTS.md` for required agent workflow and documentation stewardship rules.
- `agents/00_overview.md` for validator concepts and runtime model.
- `agents/02_specification.md` for execution, scheduler, commit, undelegation, Magic Actions, ephemeral accounts, RPC/router, and recovery behavior.
- `agents/03_architecture.md` for cross-crate boundaries and hot-path architecture.
- `agents/04_crate-map.md` for workspace crate ownership and consumers.
- `agents/05_testing-and-validation.md` for baseline validation commands and integration suites.
- `docs/architecture.md` section `magicblock-core — the wiring loom` for additional architecture context.
- Consumer-specific guides such as `agents/crates/magicblock-api.md`, `agents/crates/magicblock-aperture.md`, `agents/crates/magicblock-account-cloner.md`, `agents/crates/magicblock-accounts.md`, `agents/crates/magicblock-chainlink.md`, and `agents/crates/magicblock-config.md`.
