# `magicblock-committor-service`

## Purpose

`magicblock-committor-service` is the validator-side settlement service that turns Magic Program scheduled intent bundles into Solana base-layer transactions. It executes commits, commit-and-undelegates, commit-finalizes, undelegates, and Magic Actions by building atomic tasks, packing them into transactions, preparing delivery resources such as buffers and address lookup tables, sending transactions through `magicblock-rpc-client`, and persisting status for operator queries and restart recovery.

High-level responsibilities:

- expose `CommittorService` / `BaseIntentCommittor` as the async service boundary used by `magicblock-api`, `magicblock-accounts`, and account cloning;
- schedule intent bundles without executing mutually conflicting committed accounts in parallel;
- fetch Delegation Program metadata, commit nonces, rent reimbursements, and base accounts needed for task construction;
- choose commit delivery strategies: state args, diff args, state buffers, diff buffers, and optional ALTs;
- prepare and clean up committor-program buffer accounts and TableMania lookup-table reservations;
- execute single-stage or two-stage base-layer transaction flows and schedule action callbacks;
- persist commit rows, strategies, signatures, and pending intents in SQLite for status APIs and recovery.

This crate is on the base-layer settlement hot path. Changes can affect fund safety, undelegation liveness, commit ordering/nonces, restart recovery, RPC load, transaction count, and latency. Security and correctness take priority over throughput: do not weaken signer usage, base-layer freshness/min-context-slot handling, commit nonce sequencing, scheduler conflict blocking, or buffer/ALT cleanup safety.

End-to-end commit/undelegation semantics live in .agents/specs/validator-specification.md; this crate owns validator-side intent scheduling, task strategy, delivery preparation, persistence, and settlement execution.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-committor-service` change. In particular, update it for changes to:

- `CommittorService`, `BaseIntentCommittor`, `CommittorServiceExt`, channel messages, startup/shutdown, or cancellation semantics;
- `ChainConfig`, `ComputeBudgetConfig`, action timeout behavior, RPC/websocket construction, or configured commitment assumptions;
- intent scheduling, conflict detection, executor concurrency, backlog capacity, result broadcasting, or metrics;
- `TaskInfoFetcher` commit nonce caching, `min_context_slot` behavior, retry policy, or cache reset rules;
- task building, commit/finalize/undelegate/action task semantics, commit nonce persistence, diff/state thresholds, or rent reimbursement fetches;
- strategy selection, transaction-size limits, buffer/ALT fallback, single-stage versus two-stage choice, or action-stripping/retry logic;
- delivery preparation, committor-program buffer initialization/write/cleanup, TableMania reservations, or RPC send/retry/error mapping;
- SQLite schema, persisted statuses/strategies/signatures, pending-intent recovery windows, or recovery reconstruction;
- integration test commands, performance characteristics, or operator-facing diagnostics.


For the general documentation-update rule, see .agents/memory/agent-memory-and-docs.md.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-committor-service/Cargo.toml` | Package metadata and dependencies on committor program, core, Magic Program, metrics, RPC client, TableMania, SQLite, and Solana crates. |
| `magicblock-committor-service/README.md` | High-level architecture notes for intent execution, schedulers, task builders, strategist, and delivery preparation. |
| `src/lib.rs` | Public crate surface. Re-exports `ComputeBudgetConfig`, `DEFAULT_ACTIONS_TIMEOUT`, committor-program changeset types, `BaseIntentCommittor`, and `CommittorService`. |
| `src/service.rs` | Actor-style service handle, `CommittorMessage`, `CommittorService::try_start`, `BaseIntentCommittor` trait, oneshot request API, and cancellation token. |
| `src/service_ext.rs` | `CommittorServiceExt` wrapper that waits for broadcasted execution results by intent id. Used in tests and synchronous-style callers. |
| `src/config.rs` and `src/compute_budget.rs` | Chain/RPC configuration, default action timeout, and per-task compute-budget helpers. |
| `src/committor_processor.rs` | Constructs `MagicblockRpcClient`, `TableMania`, `IntentPersisterImpl`, `IntentExecutionManager`, and `CacheTaskInfoFetcher`; exposes persistence queries and recovery helpers. |
| `src/intent_execution_manager.rs` | Backpressure boundary between service and execution engine; enqueues bundles and falls back to an internal DB when the channel is full. |
| `src/intent_execution_manager/intent_execution_engine.rs` | Main scheduler loop, executor semaphore (`MAX_EXECUTORS = 50`), result broadcasting, metrics, and successful-cleanup spawning. |
| `src/intent_execution_manager/intent_scheduler.rs` | Pubkey conflict scheduler for committed accounts. Maintains FIFO blocking queues and prevents duplicate/concurrent conflicting intents. |
| `src/intent_executor/` | Intent execution state machine, transaction client, factory, single-stage/two-stage executors, timeout helpers, and commit nonce fetcher/cache. |
| `src/tasks/` | Atomic base-layer task types and task builders/strategist for commit, commit-finalize, undelegate, actions, buffers, ALTs, and compute budgets. |
| `src/transaction_preparator/` | Converts a `TransactionStrategy` into a `VersionedMessage` after preparing buffers and lookup tables; owns buffer/ALT cleanup. |
| `src/persist/` | SQLite persistence for commit rows, bundle signatures, status/strategy enums, and conversion utilities. |
| `src/stubs/` | Feature-gated dev/test stub committor behind `dev-context-only-utils`. |
| `magicblock-api/src/magic_validator.rs` | Starts the service at validator initialization with `committor_service.sqlite`, validator keypair, RPC URL, websocket URL, compute-unit price, and action callback scheduler. |
| `magicblock-accounts/src/scheduled_commits_processor.rs` | Main runtime producer/consumer: takes scheduled intent bundles from the transaction scheduler, schedules them with the committor, consumes result broadcasts, and performs pending-intent recovery after ledger replay. |
| `magicblock-account-cloner/src/account_cloner.rs` | Uses `BaseIntentCommittor` for lookup-table reservation around account cloning and diagnostic mapping of committor errors. |
| `magicblock-api/src/magic_sys_adapter.rs` | Fetches current commit nonces through the committor service for Magic syscalls. |
| `test-integration/test-committor-service/` | Integration coverage for delivery preparators, transaction preparators, intent executor flows, and local commit execution. |

Main upstream dependencies:

- `magicblock-program` / `magicblock-magic-program-api` for `ScheduledIntentBundle`, intent bundle structure, validator authority, and Magic Action types;
- `magicblock-committor-program` for buffer/chunks instruction builders and changeset types;
- `magicblock-delegation-program-api` for delegation metadata PDA derivation and commit nonce/rent reimbursement reads;
- `magicblock-rpc-client` for base-layer sends, confirmations, account reads, transaction diagnostics, slot/blockhash caching, and `min_context_slot` RPC calls;
- `magicblock-table-mania` for ALT reservation, finalized table fetch, release, and GC;
- `magicblock-core` for committed-account types and `ActionsCallbackScheduler`.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exports:

- `pub mod config`, `error`, `intent_execution_manager`, `intent_executor`, `persist`, `service_ext`, `tasks`, `transaction_preparator`, and `transactions`;
- `ComputeBudgetConfig` and `DEFAULT_ACTIONS_TIMEOUT`;
- `ChangedAccount`, `Changeset`, and `ChangesetMeta` re-exported from `magicblock-committor-program`;
- `BaseIntentCommittor` and `CommittorService`.

Most modules are public for tests and consumers, but the intended runtime boundary is the service trait plus status/query helpers. Avoid adding new cross-crate call paths into internals unless the ownership boundary is intentional and documented.

### `CommittorService` and `BaseIntentCommittor`

`CommittorService::try_start(authority, persist_file, chain_config, chain_slot, actions_callback_executor)` creates an mpsc-backed actor with capacity `1_000` and spawns it on Tokio. The actor owns a `CommittorProcessor`, and each public method sends a `CommittorMessage` plus an oneshot response channel. `try_send` logs if the actor channel is full or closed; it does not block the caller.

`BaseIntentCommittor` is the shared trait used by runtime consumers and stubs. Important methods:

- `reserve_pubkeys_for_committee(committee, owner)` reserves committee-specific pubkeys in TableMania before cloning/use;
- `schedule_intent_bundles(Vec<ScheduledIntentBundle>)` schedules fresh intents and persists rows first;
- `subscribe_for_results()` returns a broadcast receiver of `BroadcastedIntentExecutionResult` values;
- `get_commit_statuses(message_id)` and `get_commit_signatures(commit_id, pubkey)` query SQLite status/signature data;
- `get_transaction(signature)` fetches base-layer transaction diagnostics;
- `fetch_current_commit_nonces(pubkeys, min_context_slot)` returns current base-layer nonces without incrementing the cache;
- `stop()` cancels the actor; `stopped()` resolves when cancellation is requested.

`CommittorService` also exposes inherent helpers not on the trait: common ALT reservation/release, `get_pending_intent_bundles`, `schedule_recovered_intent_bundles`, `get_lookup_tables`, and a blocking-channel `fetch_current_commit_nonces_sync` used where an async oneshot is inconvenient.

### `CommittorServiceExt`

`CommittorServiceExt<CC>` wraps any `BaseIntentCommittor`, subscribes to broadcast results once, and dispatches results to one pending oneshot per intent id. `schedule_intent_bundles_waiting` registers all intent ids before scheduling and rejects duplicate ids with `RepeatingMessageError`. This prevents a fast execution result from being broadcast before the waiter exists.

Do not use duplicate intent ids with the extension: `pending_messages` is keyed only by `ScheduledIntentBundle::id`.

### Config and compute budgets

`ChainConfig` stores RPC URI, optional websocket URI, Solana commitment, `ComputeBudgetConfig`, and `actions_timeout` (`DEFAULT_ACTIONS_TIMEOUT = 60s`). The validator currently constructs it in `magicblock-api` with confirmed base-layer commitment and the configured commit compute-unit price.

`ComputeBudgetConfig::new(compute_unit_price)` controls budgets for args processing, buffer close, buffer process-and-close, finalize, undelegate, buffer init/realloc, and buffer writes. Buffer init/realloc/write budgets currently hard-code `compute_unit_price: 1_000_000` rather than the caller-provided price; treat that as current behavior when validating fee/priority-fee changes.

### Persistence API

`IntentPersister` is the internal persistence trait. `IntentPersisterImpl` wraps `CommittsDb` behind `Arc<Mutex<_>>` and creates two tables:

- `commit_status`, keyed by `(message_id, commit_id, pubkey)`, storing account owner, slot, ER blockhash, undelegate flag, lamports/data, commit type, status, strategy, signatures, timestamps, and retry count;
- `bundle_signature`, keyed by bundle/message id, storing commit-stage and finalize-stage signatures.

`IntentPersisterImpl::create_commit_rows` creates one row per committed/undelegated account. Empty data is persisted as `CommitType::EmptyAccount` with `data = None`; non-empty data is persisted as `CommitType::DataAccount`.

## Runtime flows

### Startup and service wiring

```text
magicblock-api::MagicValidator::init_committor_service
  -> CommittorService::try_start
  -> CommittorActor::try_new
  -> CommittorProcessor::try_new
     -> MagicblockRpcClient from RPC/websocket/chain_slot
     -> TableMania with default GC
     -> IntentPersisterImpl at storage/committor_service.sqlite
     -> CacheTaskInfoFetcher<RpcTaskInfoFetcher>
     -> IntentExecutionManager + IntentExecutionEngine
  -> actor run loop spawned on Tokio
```

The service is initialized before the account manager starts pending-intent recovery. Pending recovery must run after ledger replay so local accounts reflect delegated state before recovered intents are checked.

### Fresh scheduled intent flow

```text
Magic Program schedules intent in ER
  -> transaction scheduler exposes ScheduledIntentBundle(s)
  -> magicblock-accounts::ScheduledCommitsProcessor::process
  -> CommittorService::schedule_intent_bundles
  -> CommittorProcessor::schedule_intent_bundle
     -> IntentPersisterImpl::start_base_intents
     -> IntentExecutionManager::schedule
     -> IntentExecutionEngine::main_loop
     -> IntentScheduler blocks conflicts by committed pubkeys
     -> IntentExecutorImpl executes selected intent
     -> broadcast result
  -> ScheduledCommitsProcessor consumes result and updates local/metadata state
```

`CommittorProcessor::schedule_intent_bundle` logs persistence failures but still tries to execute. This is intentionally loud because losing persistence weakens restart recovery; do not hide or downgrade that error path.

### Recovery flow for pending intents

1. `magicblock-accounts` calls `get_pending_intent_bundles()` after replay.
2. `CommittorProcessor::pending_intent_bundles` loads SQLite rows with `CommitStatus::Pending` and `created_at` inside the 14-day recovery window.
3. It fetches the current base-layer slot and reconstructs `ScheduledIntentBundle`s grouped by `message_id`.
4. Rows for a message must agree on ER slot and ER blockhash; otherwise that message is skipped.
5. Data-account rows without stored data are skipped because they cannot reconstruct a `CommittedAccount` safely.
6. `magicblock-accounts` filters recovered bundles against current delegated state, then calls `schedule_recovered_intent_bundles` so rows are not inserted again.

Preserve the no-repersist path for recovered intents. Re-inserting rows can violate primary keys or duplicate status history.

### Scheduling and concurrency flow

`IntentExecutionManager::schedule` first checks whether its internal DB/backlog is empty. If it is not empty, new bundles are stored there to preserve order. If the channel is full, the current and remaining bundles are also stored in the DB. The current `DummyDB` is in-memory; durable recovery is handled by SQLite commit rows, not this backlog.

`IntentExecutionEngine` repeatedly:

1. handles completed executor join handles first, which lets blocked intents become eligible before accepting new ones;
2. receives a new bundle from the channel or DB if scheduler capacity allows;
3. asks `IntentScheduler` whether it can run now;
4. waits for one of `MAX_EXECUTORS = 50` semaphore permits;
5. creates an executor and spawns intent execution;
6. broadcasts the result, completes the scheduler entry, and cleans buffers/ALTs only after successful execution.

The scheduler blocks on the union of `ScheduledIntentBundle::get_all_committed_pubkeys()`, including commit and commit-and-undelegate accounts in the same bundle. Standalone base actions with no committed pubkeys do not block on account keys.

### Intent execution and task strategy flow

```text
IntentExecutorImpl::execute
  -> mark persisted rows Pending
  -> TaskBuilderImpl::commit_tasks + finalize_tasks
     -> fetch next commit nonces and diffable base accounts using max(remote_slot)
     -> persist commit_id for each committed account
     -> create commit, commit-finalize, undelegate, finalize, and action tasks
  -> tag intents whose commit uses nonce <= 1 with a per-intent uniqueness noop
  -> TaskStrategist::build_execution_strategy
     -> try single transaction when total task count <= 22 and it fits
     -> optimize large tasks to buffers when needed
     -> use ALTs when buffers alone do not fit
     -> choose two-stage when single-stage is too large or ALT latency would be worse
  -> TransactionPreparator prepares buffers/ALTs and assembles VersionedMessage
  -> SingleStageExecutor or TwoStageExecutor sends base-layer transactions
  -> persist final status/signatures and schedule callbacks
  -> reset nonce cache for all committed pubkeys on errors, or only undelegated pubkeys on successful undelegation
```

For committed accounts with `data.len() > COMMIT_STATE_SIZE_THRESHOLD` (`256`), the task builder fetches the base account and may use diff-in-args delivery. If the base-account fetch fails, it falls back to full state args and logs a warning. This can increase transaction size and trigger buffer/ALT strategy later.

### Delivery preparation and cleanup flow

`TransactionPreparatorImpl::prepare_for_strategy` first compiles against dummy lookup tables to fail early if the message cannot fit. It then calls `DeliveryPreparator::prepare_for_delivery`:

1. prepare each task concurrently, recording task-preparation metrics;
2. for buffer tasks, persist `BufferAndChunkPartiallyInitialized`, initialize/realloc buffer accounts, persist `BufferAndChunkInitialized`, write missing chunks, then persist `BufferAndChunkFullyInitialized`;
3. if a buffer account is already initialized, cleanup is attempted, the cached blockhash is invalidated, and preparation is retried once;
4. reserve ALTs in TableMania and wait for finalized lookup table accounts;
5. assemble the final versioned message with real lookup table accounts.

Cleanup closes prepared buffers and releases TableMania pubkeys. `IntentExecutionEngine` intentionally runs cleanup only after successful execution because failed intent cleanup can race with a retried or concurrent intent using the same buffer PDA set.

## Important internals and caveats

### Commit nonce cache

`CacheTaskInfoFetcher` caches commit nonces in a 10,000-entry LRU. It uses per-pubkey async mutexes acquired in sorted order to avoid A->B / B->A deadlocks, and a `retiring` map to keep evicted locks alive while in-flight requests still hold them. `fetch_next_commit_nonces` increments cached values and reserves the next nonce; `fetch_current_commit_nonces` reads/stores the current value without incrementing.

`IntentExecutorImpl` resets cached nonces according to execution certainty. On any execution error, it resets all committed pubkeys because it cannot know what landed on chain. On successful undelegation paths, it resets only the pubkeys returned by `get_undelegate_intent_pubkeys()` and `get_commit_finalize_and_undelegate_intent_pubkeys()`. Other successfully committed pubkeys keep their incremented cached nonce, which avoids a chain re-fetch racing the just-landed finalize and reusing a stale nonce/buffer PDA.

Do not remove sorted lock acquisition or the retiring map without replacing the deadlock/race prevention. Commit nonce races can cause base-layer commit failures and stuck undelegations.

### `min_context_slot` and freshness

Task-info RPC reads use the maximum `remote_slot` across committed accounts as `min_context_slot` when fetching delegation metadata and diffable base accounts. This helps avoid building commits against base-layer state older than the ER account snapshot. The fetcher retries `Minimum context slot not reached` up to five times with short sleeps. Preserve this freshness check unless the broader account-sync/settlement contract changes.

### Persistence is both status API and recovery state

SQLite rows are used by operator/status APIs and by restart recovery. Updating status mapping is not a cosmetic change: it affects which intents are recoverable, which accounts look failed/stuck, and which signatures are returned. Keep persisted enum string conversions compatible with existing rows.

### Buffers, ALTs, and transaction fit

`TaskStrategist` first tries args, then buffer optimization, then ALTs. It chooses two-stage execution in cases where a single-stage ALT transaction would be slower than two no-ALT transactions. Altering thresholds such as `MAX_UNITED_TASKS_LEN = 22`, `COMMIT_STATE_SIZE_THRESHOLD = 256`, transaction-size constants, or buffer chunking changes latency, RPC transaction counts, and fit behavior.

### Per-intent uniqueness noop

Intent transactions are otherwise built from fully deterministic inputs. After an undelegate/re-delegate cycle the delegation metadata nonce restarts, so a first commit (nonce 1) can be byte-identical to a prior instance's landed transaction: identical bytes yield the identical signature, the skip-preflight send is deduped by the network, and the status-based confirmer matches the old transaction — the intent reports success without executing. `IntentExecutorImpl::execute_inner` therefore passes `Some(intent_id)` as `uniqueness_nonce` to `TaskStrategist` for intents whose commit uses nonce <= 1 (and always for standalone actions). The strategist renders it as a constant-size spl-noop instruction carrying the intent id on every produced stage (the finalize stage carries no nonce bytes and aliases the same way) and includes it in all fit checks. Retries of the same intent keep the same id, preserving intentional dedup. Commit-id recovery (`handle_commit_id_error`) re-tags the strategy when a stale-cache retry lands back on nonce 1. The noop program must exist on the base layer (deployed on mainnet/devnet; loaded from `test-integration/schedulecommit/elfs/noop.so` in integration configs).

### Actions and callbacks

Standalone actions are currently built through commit-task paths even when there are no committed accounts. Base actions with callbacks are extracted and scheduled through the `ActionsCallbackScheduler`. `actions_timeout` applies across action-related execution work. If action execution fails with recoverable CPI/limit errors, the executor can strip actions or move from single-stage to two-stage depending on the path; preserve error visibility through `patched_errors` and callback reports.

### Service channel backpressure

The public service API uses nonblocking `try_send`. If the service channel is full or closed, callers receive only a oneshot that may never be answered while an error is logged. This is current behavior; changing it to fail synchronously would be a public contract change that needs consumer updates.

## Important invariants

1. Do not execute two intent bundles concurrently when their committed-pubkey sets overlap.
2. Preserve FIFO blocking semantics across indirectly blocked intents; later intents must not bypass an earlier blocked intent sharing any key.
3. Do not schedule duplicate intent ids in the same scheduler/execution-extension context.
4. Commit nonces must be fetched with base-layer freshness (`min_context_slot`) and incremented atomically per account.
5. Execution errors must reset cached nonces for all committed pubkeys; successful undelegation must reset only the undelegated pubkeys and preserve other committed-account cache entries.
6. Fresh intent scheduling must persist rows before execution when possible; recovered scheduling must not reinsert rows.
7. Pending-intent recovery must reconstruct only rows inside the recovery window and skip inconsistent or incomplete persisted groups.
8. Buffer accounts and ALTs must be prepared before transaction assembly uses them, and released/closed only when safe.
9. Failed intent cleanup must not race with retries using the same buffer PDAs; current cleanup is success-only for that reason.
10. Transaction-size and compute-budget choices must keep produced transactions under Solana wire limits.
11. Base-layer sends must preserve explicit processed/committed confirmation semantics from `magicblock-rpc-client`.
12. Intents whose commit uses nonce <= 1 must carry the per-intent uniqueness noop on every stage; otherwise their transactions can alias a prior delegation instance's landed signature and report success without executing.
12. Signer/authority requirements for validator-signed commits, committor-program buffers, ALTs, callbacks, and base-layer instructions must not be relaxed.
13. Persistence status/signature updates must continue to expose enough information for diagnostics, retries, and recovery.
14. Avoid adding blocking I/O or unbounded work to service actor, scheduler, executor, task-preparation, or RPC hot paths.

## Common change areas and what to inspect

### Changing service API, startup, or shutdown

Start with `src/service.rs`, `src/committor_processor.rs`, and `magicblock-api/src/magic_validator.rs`. Then inspect `magicblock-accounts/src/scheduled_commits_processor.rs`, `magicblock-account-cloner/src/account_cloner.rs`, and `magicblock-api/src/magic_sys_adapter.rs`. Check oneshot behavior, channel capacity/backpressure, cancellation, and whether consumers need errors instead of logged-only failures.

### Changing scheduling or concurrency

Start with `src/intent_execution_manager/intent_scheduler.rs`, `intent_execution_engine.rs`, and tests in those files. Verify conflict sets include all committed accounts in mixed bundles, scheduler capacity remains bounded, semaphore permits are always released, and completion cannot corrupt blocked queues.

### Changing commit nonce or metadata fetching

Start with `src/intent_executor/task_info_fetcher.rs` and `src/tasks/task_builder.rs`. Inspect `magicblock-api/src/magic_sys_adapter.rs` for current nonce queries. Preserve sorted lock acquisition, cache reset behavior, `min_context_slot`, Delegation Program PDA derivation, and retry/error classification.

### Changing task construction or strategy selection

Start with `src/tasks/task_builder.rs`, `src/tasks/task_strategist.rs`, `src/tasks/commit_task.rs`, `src/tasks/commit_finalize_task.rs`, and `src/tasks/utils.rs`. Then inspect `magicblock-committor-program` instruction builders, `magicblock-delegation-program-api` expectations, and integration tests under `test-integration/test-committor-service`. Validate commit ids, allow-undelegation flags, action ordering, diff-vs-state delivery, buffer conversion, ALT keys, and strategy persistence.

### Changing delivery preparation or cleanup

Start with `src/transaction_preparator/mod.rs` and `delivery_preparator.rs`, then inspect `.agents/context/crates/magicblock-committor-program.md`, `.agents/context/crates/magicblock-table-mania.md`, and `.agents/context/crates/magicblock-rpc-client.md`. Check buffer init/realloc/write chunking, retry handling for already-initialized buffers, cached blockhash invalidation, ALT finalized waits, cleanup-on-success only, and release of TableMania refs.

### Changing persistence or recovery

Start with `src/persist/db.rs`, `src/persist/commit_persister.rs`, `src/persist/types/`, and `src/committor_processor.rs` recovery helpers. Then inspect `magicblock-accounts/src/scheduled_commits_processor.rs`. Preserve schema compatibility, enum string values, `u64`/`i64` conversions, row grouping by `message_id`, 14-day recovery window, and no-repersist recovery scheduling.

### Changing metrics or observability

Start with metric calls in `intent_execution_engine.rs`, `delivery_preparator.rs`, and `intent_execution_client.rs`. Keep this guide focused on local instrumentation intent; metric naming, labels, and registry details belong in `.agents/context/crates/magicblock-metrics.md`.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-committor-service`.
- Relevant integration suites: `test-committor`, including preparators, ix-order, ix-multi, commit-finalize, intent-executor, and recovery targets; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Related suite intent: when TableMania or RPC-client behavior is touched, include the TableMania suite or focused committor preparation/delivery coverage.
- Performance/security validation intent: report effects on executor parallelism, RPC calls, transaction count, ALT waits, buffer writes/chunks, SQLite writes, and cleanup latency; confirm signer/authority requirements, `min_context_slot` freshness, nonce sequencing, scheduler conflict blocking, and recovery durability remain intact.


## Adjacent implementation references

- `.agents/context/crates/magicblock-committor-program.md` — buffer/chunks on-chain helper contracts.
- `.agents/context/crates/magicblock-rpc-client.md` — base-layer send/confirm and RPC helper behavior.
- `.agents/context/crates/magicblock-table-mania.md` — ALT lifecycle and finalized-read semantics.
- `.agents/context/crates/magicblock-accounts.md` — scheduled commit processing and pending-intent recovery call sites.
- `magicblock-committor-service/README.md` — high-level implementation notes.
- `test-integration/test-committor-service/` — integration coverage of delivery and intent execution.
