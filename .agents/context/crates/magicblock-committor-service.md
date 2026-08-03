# `magicblock-committor-service`

## Purpose

`magicblock-committor-service` is the validator-side settlement service that turns Magic Program scheduled intent bundles into Solana base-layer transactions. It executes commits, commit-and-undelegates, commit-finalizes, undelegates, and Magic Actions by building atomic tasks, packing them into transactions, preparing delivery resources such as buffers and address lookup tables, sending transactions through `magicblock-rpc-client`, and driving each intent through its execution lifecycle.

Intent durability follows an Outbox pattern: each scheduled intent is represented on-chain as an outbox intent PDA (owned by the outbox intent program, seeded by intent id) tracked in `AccountsDb`. The service advances that PDA's status as the intent moves through acceptance and execution and closes it on completion; restart recovery reconstructs pending intents by scanning outbox intent accounts rather than reading a local store.

High-level responsibilities:

- expose `CommittorProcessor` / `IntentExecutionService` as the async service boundary used by `magicblock-api` and account cloning;
- schedule intent bundles without executing mutually conflicting committed accounts in parallel;
- fetch Delegation Program metadata, including commit nonces and rent payer data, plus base accounts needed for task construction;
- choose commit delivery strategies: state args, diff args, state buffers, diff buffers, and optional ALTs;
- prepare and clean up committor-program buffer accounts and TableMania lookup-table reservations;
- execute single-stage or two-stage base-layer transaction flows and schedule action callbacks;
- advance and close each intent's outbox intent PDA via `OutboxClient` (`set_intent_execution_stage`, `notify_commit_sent`, `close_intent`), and recover pending intents at startup by scanning outbox intent accounts in `AccountsDb`.

This crate is on the base-layer settlement hot path. Changes can affect fund safety, undelegation liveness, commit ordering/nonces, restart recovery, RPC load, transaction count, and latency. Security and correctness take priority over throughput: do not weaken signer usage, base-layer freshness/min-context-slot handling, commit nonce sequencing, scheduler conflict blocking, or buffer/ALT cleanup safety.

End-to-end commit/undelegation semantics live in .agents/specs/validator-specification.md; this crate owns validator-side intent scheduling, task strategy, delivery preparation, persistence, and settlement execution.

## Update requirement

Queue an update to this guide for the weekly documentation-maintenance task whenever behavior or contracts in `magicblock-committor-service` change. Include changes to:

- `CommittorProcessor`, `IntentExecutionService`, scheduling/result-dispatch semantics, startup/shutdown, or cancellation;
- `ChainConfig`, `ComputeBudgetConfig`, action timeout behavior, RPC/websocket construction, or configured commitment assumptions;
- intent scheduling, conflict detection, executor concurrency, backlog capacity, result broadcasting, or metrics;
- `TaskInfoFetcher` commit nonce caching, `min_context_slot` behavior, retry policy, or cache reset rules;
- task building, commit/finalize/undelegate/action task semantics, commit nonce persistence, diff/state thresholds, or rent reimbursement fetches;
- strategy selection, transaction-size limits, buffer/ALT fallback, single-stage versus two-stage choice, or action-stripping/retry logic;
- delivery preparation, committor-program buffer initialization/write/cleanup, TableMania reservations, or RPC send/retry/error mapping;
- outbox intent PDA lifecycle and layout, the `OutboxClient` contract (`accept_scheduled_intents`, `set_intent_execution_stage`, `notify_commit_sent`, `close_intent`, `outbox_reader`), or AccountsDb-scan-based pending-intent recovery;
- integration test commands, performance characteristics, or operator-facing diagnostics.


For the general documentation-update rule, see .agents/memory/agent-memory-and-docs.md.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-committor-service/Cargo.toml` | Package metadata and dependencies on committor program, core, Magic Program, metrics, RPC client, TableMania, AccountsDb, and Solana crates. |
| `magicblock-committor-service/README.md` | High-level architecture notes for intent execution, schedulers, task builders, strategist, and delivery preparation. |
| `src/lib.rs` | Public crate surface. Re-exports `ComputeBudgetConfig`, `DEFAULT_ACTIONS_TIMEOUT`, and committor-program changeset types. |
| `src/config.rs` and `src/compute_budget.rs` | Chain/RPC configuration, default action timeout, and per-task compute-budget helpers. |
| `src/committor_processor.rs` | Constructs `MagicblockRpcClient`, `TableMania`, `IntentEngineHandle`, and `CacheTaskInfoFetcher`. `CommittorProcessor<D: BacklogDB>` exposes `schedule_intent_bundles`, `execute_intent_bundles`, `subscribe_for_results`, and `fetch_current_commit_nonces` directly as async methods. |
| `src/intent_engine.rs` and `src/intent_engine/intent_channel.rs` | `IntentEngineHandle` wraps the executor factory and spawns `IntentExecutionEngine`. `IntentScheduleHandle`/`IntentStream` form the scheduling channel: bundles go to an mpsc channel when it has room, otherwise to the `BacklogDB` backlog, which is drained before the channel is polled again to preserve arrival order. |
| `src/intent_engine/db.rs` | `BacklogDB` trait plus `DummyIntentBacklog` (production: stores intent ids and re-reads the intent from `AccountsDb` on pop) and `DummyDB` (test-only, in-memory). This backlog only smooths bursts past channel capacity — it is not the intent durability mechanism. |
| `src/intent_engine/intent_execution_engine.rs` | Main scheduler loop, executor semaphore (`MAX_EXECUTORS = 50`), transient-failure intent retries (`MAX_INTENT_ATTEMPTS = 3` with jittered linear backoff, bounded by a `MAX_SLEEPING_RETRIERS = 5_000` semaphore), result broadcasting, metrics, and per-attempt cleanup spawning. |
| `src/intent_engine/intent_scheduler.rs` | Pubkey conflict scheduler for committed accounts. Maintains FIFO blocking queues and prevents duplicate/concurrent conflicting intents. |
| `src/intent_executor/` | Intent execution state machine, transaction client, factory (`ExecutorConfig`/`IntentExecutorBuilderImpl`), single-stage/two-stage executors, and timeout helpers. |
| `src/tasks/` | Atomic base-layer task types, task builders/strategist for commit, commit-finalize, undelegate, actions, buffers, ALTs, and compute budgets, plus `task_info_fetcher.rs` (commit nonce fetcher/cache). |
| `src/transaction_preparator/` | Converts a `TransactionStrategy` into a `VersionedMessage` after preparing buffers and lookup tables; owns buffer/ALT cleanup. |
| `src/outbox/` | `OutboxClient` trait and `ScheduledBaseIntentMeta`/`IntentSentTransaction` (`mod.rs`); `InternalOutboxClient` production implementation that submits `set_intent_execution_stage`/`notify_commit_sent`/`close_intent` transactions and runs `accept_scheduled_intents` (`outbox_client.rs`); `InternalOutboxIntentBundlesReader`, which scans `AccountsDb` for outbox intent PDAs owned by the outbox intent program (`outbox_intent_bundles_reader.rs`). |
| `magicblock-api/src/magic_validator.rs` | Starts the service at validator initialization: builds `InternalOutboxClient` and `CommittorProcessor<DummyIntentBacklog>`, then either `IntentExecutionService::disabled()` in replica mode or `IntentExecutionService::new(...)` otherwise, wires `MagicSysAdapter` to the processor for commit-nonce syscalls. |
| `src/service.rs` | `IntentExecutionService<O, D>` (`Created`/`Started`/`Stopped`/`Disabled`/`Error` states) and `ServiceInner`: on start, recovers pending intents by scanning the outbox before accepting new ones, then periodically calls `OutboxClient::accept_scheduled_intents` and schedules the result with `CommittorProcessor`. |
| `magicblock-api/src/magic_sys_adapter.rs` | Fetches current commit nonces through the committor service for Magic syscalls. |
| `test-integration/test-committor-service/` | Integration coverage for delivery preparators, transaction preparators, intent executor flows, and local commit execution. |

Main upstream dependencies:

- `magicblock-program` / `magicblock-magic-program-api` for `ScheduledIntentBundle`, intent bundle structure, validator authority, and Magic Action types;
- `magicblock-committor-program` for buffer/chunks instruction builders and changeset types;
- `magicblock-delegation-program-api` for delegation metadata PDA derivation and commit nonce/rent reimbursement reads;
- `magicblock-rpc-client` for base-layer sends, confirmations, account reads, transaction diagnostics, slot/blockhash caching, and `min_context_slot` RPC calls;
- `magicblock-table-mania` for ALT reservation, finalized table fetch, release, and GC;
- `magicblock-accounts-db` for scanning/reading outbox intent PDAs during recovery and backlog pop;
- `magicblock-core` for committed-account types and `ActionsCallbackScheduler`.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exports:

- `pub mod committor_processor`, `config`, `error`, `intent_engine`, `intent_executor`, `outbox`, `service`, `tasks`, `transaction_preparator`, and `utils` (plus `test_utils` under `#[cfg(test)]`);
- `ComputeBudgetConfig` and `DEFAULT_ACTIONS_TIMEOUT`;
- `ChangedAccount`, `Changeset`, and `ChangesetMeta` re-exported from `magicblock-committor-program`.

Most modules are public for tests and consumers, but the intended runtime boundary is the service trait plus status/query helpers. Avoid adding new cross-crate call paths into internals unless the ownership boundary is intentional and documented.

### `CommittorProcessor`

`CommittorProcessor::new(authority, chain_config, chain_slot, db: D, outbox_client, actions_callback_executor)` builds `MagicblockRpcClient`, `TableMania`, and an `IntentEngineHandle<D>`, then spawns a `dispatcher` task that pairs broadcast execution results with pending callers. There is no actor/message-channel indirection — callers invoke async methods directly:

- `schedule_intent_bundles(Vec<OutboxIntentBundle>)` hands bundles to the `IntentEngineHandle` for scheduling and returns once they are accepted into the engine/backlog;
- `execute_intent_bundles(Vec<OutboxIntentBundle>)` registers one oneshot listener per intent id in `pending_result_listeners`, schedules the bundles, and awaits all listeners; duplicate ids in flight are rejected with `RepeatingMessageError`;
- `subscribe_for_results()` returns a broadcast receiver of `BroadcastedIntentExecutionResult` values;
- `fetch_current_commit_nonces(pubkeys, min_context_slot)` returns current base-layer nonces without incrementing the cache.

The background `dispatcher` task consumes the broadcast result stream and forwards each result to the matching oneshot in `pending_result_listeners`, if one is registered; results with no waiter (fire-and-forget scheduling) are dropped after being broadcast.

### `IntentExecutionService`

`IntentExecutionService<O, D>` is a state machine (`Created`, `Started`, `Stopped`, `Disabled`, `Error`). `disabled()` makes `start`/`stop` no-ops and is used outside `CoordinationMode::Primary` (replica mode), where the accept/schedule loop must never run. `start()`/`stop()` transition `Created` <-> `Started` <-> `Stopped`, spawning/joining the `ServiceInner::accept_worker` task.

`ServiceInner::accept_worker` first calls `reschedule_intents()` (see Recovery flow below), then loops on a `slot_interval` ticker calling `OutboxClient::accept_scheduled_intents()` and scheduling the result through `CommittorProcessor::schedule_intent_bundles`. Bundles whose intent touches an undelegating pubkey subscribe that pubkey with `chainlink` before scheduling.

### Config and compute budgets

`ChainConfig` stores RPC URI, optional websocket URI, Solana commitment, `ComputeBudgetConfig`, and `actions_timeout` (`DEFAULT_ACTIONS_TIMEOUT = 60s`). The validator currently constructs it in `magicblock-api` with confirmed base-layer commitment and the configured commit compute-unit price.

`ComputeBudgetConfig::new(compute_unit_price)` controls budgets for args processing, buffer close, buffer process-and-close, finalize, undelegate, buffer init/realloc, and buffer writes. Buffer init/realloc/write budgets currently hard-code `compute_unit_price: 1_000_000` rather than the caller-provided price; treat that as current behavior when validating fee/priority-fee changes.

### `OutboxClient` and outbox intent PDAs

`OutboxClient` is the trait through which the service reads and mutates outbox intent state on-chain:

- `accept_scheduled_intents()` executes the accept transaction and returns the accepted `ScheduledIntentBundle`s;
- `set_intent_execution_stage(intent_id, stage)` advances an accepted intent's `ExecutionStage`; must be called before the corresponding base-layer transaction is sent;
- `notify_commit_sent(meta, result, execution_report)` reports an intent's execution outcome to the ER;
- `close_intent(intent_id)` closes the outbox intent PDA; only valid to call after the intent succeeded;
- `outbox_reader()` returns an `OutboxIntentBundlesReader` for scanning/fetching outbox intents.

`InternalOutboxClient` is the production implementation: it holds `AccountsDb`, an RPC client for submitting ER transactions, a `TransactionSchedulerHandle`, and a `LatestBlockProvider`. `InternalOutboxIntentBundlesReader::read(n)` buffers up to `n` outbox intents ascending by id, refilling via a `getProgramAccounts`-style scan of `AccountsDb` for accounts owned by the outbox intent program whose data starts with `OUTBOX_INTENT_DISCRIMINATOR`; `fetch_outbox_intent(intent_id)` reads a single PDA directly by its derived address.

Each outbox intent PDA holds an `OutboxIntentBundle`: the inner `ScheduledIntentBundle` plus an `OutboxIntentBundleStatus` (`Accepted` -> `Executing(ExecutionStage)` -> closed). The PDA itself, not a local table, is the durable record consumed by both status queries and restart recovery.

## Runtime flows

### Startup and service wiring

```text
magicblock-api::MagicValidator (startup)
  -> init_outbox_client -> InternalOutboxClient::new(accounts_db, rpc_client, transaction_scheduler, latest_block)
  -> init_committor_processor -> CommittorProcessor::new
     -> MagicblockRpcClient from RPC/websocket/chain_slot
     -> TableMania with default GC
     -> CacheTaskInfoFetcher<RpcTaskInfoFetcher>
     -> IntentEngineHandle + IntentExecutionEngine
     -> DummyIntentBacklog::new(accounts_db)
  -> IntentExecutionService::disabled() in replica mode, else IntentExecutionService::new(chainlink, outbox_client, committor_processor, block_time, cancellation_token)
  -> init_magic_sys(MagicSysAdapter wired to committor_processor)
```

`IntentExecutionService` is constructed after ledger replay/reset so pending recovery sees local accounts that reflect current delegated state before recovered intents are checked. In replica mode it stays `Disabled` for the validator's lifetime — the accept/schedule/recovery loop never runs.

### Fresh scheduled intent flow

```text
Magic Program schedules intent in ER
  -> MagicContext stores ScheduledIntentBundle(s)
  -> ServiceInner::accept_worker interval tick
  -> OutboxClient::accept_scheduled_intents (creates/updates outbox intent PDA(s), status = Accepted)
  -> CommittorProcessor::schedule_intent_bundles
     -> IntentEngineHandle::schedule -> mpsc channel, or BacklogDB if the channel is full
     -> IntentExecutionEngine::main_loop
     -> IntentScheduler blocks conflicts by committed pubkeys
     -> executor advances the outbox intent PDA's ExecutionStage via OutboxClient::set_intent_execution_stage, sends base-layer transaction(s)
     -> OutboxClient::notify_commit_sent, then OutboxClient::close_intent on success
     -> broadcast result
  -> CommittorProcessor::dispatcher forwards the result to any registered oneshot listener
```

### Recovery flow for pending intents

1. `ServiceInner::accept_worker` calls `reschedule_intents()` first, before entering the periodic accept loop, so outbox intents are scheduled before new ones are accepted.
2. `reschedule_intents` reads outbox intents in chunks of `RESCHEDULE_CHUNK_SIZE = 1000` via `OutboxClient::outbox_reader().read(n)`, which scans `AccountsDb` for outbox intent PDAs (see `InternalOutboxIntentBundlesReader` above). There is no age-based recovery window: every open outbox intent PDA is eligible until it is closed.
3. Each recovered bundle's `sent_transaction` is reset to `Transaction::default()`, which reports as `IntentSentTransaction::Recovered` and signals `notify_commit_sent` to rebuild the ER notification transaction with a fresh blockhash rather than reuse the stale one from before restart.
4. Recovered bundles are scheduled through the same `CommittorProcessor::schedule_intent_bundles` path as freshly accepted ones (via `process_intent_bundles`, which also subscribes any undelegating pubkeys with `chainlink`); loops continue chunk by chunk until a read returns fewer than `RESCHEDULE_CHUNK_SIZE` bundles.

Recovery does not re-create or duplicate outbox intent PDAs — it only reschedules execution for PDAs that already exist and have not been closed. Execution-time idempotency (e.g. re-checking a `PendingTransaction`'s signature before resending) is what makes re-scheduling an already-executed intent safe, not a check performed during recovery itself.

### Scheduling and concurrency flow

`IntentScheduleHandle::schedule` first checks whether its `BacklogDB` backlog is empty. If it is not empty, new bundles are stored there to preserve order. If the channel is full, the current and remaining bundles are also stored in the backlog. The production `DummyIntentBacklog` only stores intent ids in memory and re-reads each intent from `AccountsDb` on pop; it exists to preserve arrival order under backpressure, not for durability — durable recovery is the outbox intent PDA scan described above.

`IntentExecutionEngine` repeatedly:

1. handles completed executor join handles first, which lets blocked intents become eligible before accepting new ones;
2. receives a new bundle from the channel or DB if scheduler capacity allows;
3. asks `IntentScheduler` whether it can run now;
4. waits for one of `MAX_EXECUTORS = 50` semaphore permits;
5. creates an executor and spawns intent execution;
6. broadcasts the result, completes the scheduler entry, and spawns per-attempt cleanup (full buffer/ALT cleanup only after success; failed attempts release only ALT reservations).

The scheduler blocks on the union of `ScheduledIntentBundle::get_all_committed_pubkeys()`, including commit and commit-and-undelegate accounts in the same bundle. Standalone base actions with no committed pubkeys do not block on account keys.

### Intent retry policy

A failed execution is retried by the engine with a fresh executor, up to `MAX_INTENT_ATTEMPTS = 3` total attempts with jittered linear backoff, only when all of the following hold:

- the error is classified transient by `IntentExecutorError::is_transient()` (transport/RPC-side, delegated down through `TransactionStrategyExecutionError`, `TaskBuilderError`, `TransactionPreparatorError`, and ultimately `MagicBlockRpcClientError::is_transient`); deterministic failures — signer errors, oversized strategies, on-chain instruction errors, finalize failures after a landed commit (`FailedToFinalizeError` with a commit signature, `FailedFinalizePreparationError`) — are terminal on the first attempt;
- no action callbacks were scheduled during the failed attempt (a retry would double-report the intent outcome to user programs);
- the intent contains at least one commit/finalize/undelegate task, whose on-chain commit nonce makes a duplicate landing fail the retried transaction atomically; action-only intents may only retry pre-send failures (the same guard restricts their in-loop send retries to blockhash-fetch errors);
- a retry slot is free: retries release their executor permit while sleeping, and a `MAX_SLEEPING_RETRIERS = 5_000` semaphore bounds the sleeping population — without a free slot the failure is terminal.

Each attempt surrenders its strategies to the execution report (including on preparation and patch failures), so per-attempt cleanup can release partially prepared ALT reservations and buffers. A crash mid-retry remains recoverable because the intent's outbox intent PDA is still open on-chain and gets picked up by the next restart's outbox scan.

### Intent execution and task strategy flow

```text
AcceptedIntentExecutor / SingleStageIntentExecutor / TwoStageIntentExecutor::execute
  -> OutboxClient::set_intent_execution_stage advances the outbox intent PDA before each base-layer send
  -> TaskBuilderImpl::commit_tasks + finalize_tasks
     -> fetch next commit nonces, delegation metadata, and diffable base accounts using max(remote_slot)
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
  -> OutboxClient::notify_commit_sent, then OutboxClient::close_intent on success; schedule callbacks
  -> reset nonce cache for all committed pubkeys on errors, or only undelegated pubkeys on successful undelegation
```

Current DLP finalize instructions do not undelegate and require only the validator plus delegated account, so `FinalizeTask` and `CommitFinalizeTask` no longer fetch or carry owner/rent metadata. Finalize-stage `UndelegateTask`s complete ownership return through standalone DLP `Undelegate`. For owner-program requests, task building detects `DelegationMetadata.undelegation_requester = OwnerProgram` and includes the derived request PDA; DLP validates that request account and uses `DelegationMetadata.rent_payer` to close both delegation and request accounts. For validator-requested undelegation, or metadata still showing `None` because commit and finalize task lists are built before the commit-stage transaction records the validator requester on base, no request account is included.

Transaction fit is not only packet size. `TaskStrategist` currently checks whether a single-stage transaction or each two-stage transaction fits the wire size, optionally after switching commits to buffers and adding ALTs, but it does not split a transaction stage by compute units. Each commit, finalize, commit-finalize, and undelegate task currently advertises `120_000` CU, while Agave caps a transaction at `1_400_000` CU. Keep task bundles below that transaction-level cap unless the strategist and executor/output/persistence model are extended to split, record, and confirm multiple transactions for the affected stage.

For committed accounts with `data.len() > COMMIT_STATE_SIZE_THRESHOLD` (`256`), the task builder fetches the base account and may use diff-in-args delivery. If the base-account fetch fails, it falls back to full state args and logs a warning. This can increase transaction size and trigger buffer/ALT strategy later.

### Delivery preparation and cleanup flow

`TransactionPreparatorImpl::prepare_for_strategy` first compiles against dummy lookup tables to fail early if the message cannot fit. It then calls `DeliveryPreparator::prepare_for_delivery`:

1. prepare each task concurrently, recording task-preparation metrics;
2. for buffer tasks, initialize/realloc buffer accounts, then write missing chunks with retries;
3. if a buffer account is already initialized, cleanup is attempted, the cached blockhash is invalidated, and preparation is retried once;
4. reserve ALTs in TableMania and wait for finalized lookup table accounts;
5. assemble the final versioned message with real lookup table accounts.

Cleanup closes prepared buffers and releases TableMania pubkeys. `IntentExecutionEngine` intentionally runs cleanup only after successful execution because failed intent cleanup can race with a retried or concurrent intent using the same buffer PDA set.

## Important internals and caveats

### Commit nonce cache

`CacheTaskInfoFetcher` caches commit nonces in a 10,000-entry LRU. It uses per-pubkey async mutexes acquired in sorted order to avoid A->B / B->A deadlocks, and a `retiring` map to keep evicted locks alive while in-flight requests still hold them. `fetch_next_commit_nonces` increments cached values and reserves the next nonce; `fetch_current_commit_nonces` reads/stores the current value without incrementing.

Each intent executor resets cached nonces according to execution certainty. On any execution error, it resets all committed pubkeys because it cannot know what landed on chain. On successful undelegation paths, it resets only the pubkeys returned by `get_undelegate_intent_pubkeys()` and `get_commit_finalize_and_undelegate_intent_pubkeys()`. Other successfully committed pubkeys keep their incremented cached nonce, which avoids a chain re-fetch racing the just-landed finalize and reusing a stale nonce/buffer PDA.

Do not remove sorted lock acquisition or the retiring map without replacing the deadlock/race prevention. Commit nonce races can cause base-layer commit failures and stuck undelegations.

### `min_context_slot` and freshness

Task-info RPC reads use the maximum `remote_slot` across committed accounts as `min_context_slot` when fetching delegation metadata and diffable base accounts. This helps avoid building commits or standalone undelegate tasks against base-layer state older than the ER account snapshot, including stale rent-payer or owner-program requester metadata. The fetcher retries `Minimum context slot not reached` up to five times with short sleeps. Preserve this freshness check unless the broader account-sync/settlement contract changes.

### The outbox intent PDA is both the status record and the recovery source

An outbox intent's `OutboxIntentBundleStatus` is read for diagnostics and is exactly what restart recovery scans for. Changing when or how that status advances is not a cosmetic change: it affects which intents are recoverable, which accounts look failed/stuck, and which signatures are returned. Keep `ExecutionStage`/`OutboxIntentBundleStatus` transitions backward compatible with PDAs already on-chain.

### Buffers, ALTs, and transaction fit

`TaskStrategist` first tries args, then buffer optimization, then ALTs. It chooses two-stage execution in cases where a single-stage ALT transaction would be slower than two no-ALT transactions. Altering thresholds such as `MAX_UNITED_TASKS_LEN = 22`, `COMMIT_STATE_SIZE_THRESHOLD = 256`, transaction-size constants, or buffer chunking changes latency, RPC transaction counts, and fit behavior.

### Per-intent uniqueness noop

Intent transactions are otherwise built from fully deterministic inputs. After an undelegate/re-delegate cycle the delegation metadata nonce restarts, so a first commit (nonce 1) can be byte-identical to a prior instance's landed transaction: identical bytes yield the identical signature, the skip-preflight send is deduped by the network, and the status-based confirmer matches the old transaction — the intent reports success without executing. Each intent executor's `execute_inner` therefore passes `Some(intent_id)` as `uniqueness_nonce` to `TaskStrategist` for intents whose commit uses nonce <= 1 (and always for standalone actions). The strategist renders it as a constant-size spl-noop instruction carrying the intent id on every produced stage — the finalize stage needs the same protection, since without the noop its bytes contain nothing per-instance — and includes it in all fit checks. Retries of the same intent keep the same id, preserving intentional dedup. Commit-id recovery (`handle_commit_id_error`) re-tags the strategy when a stale-cache retry lands back on nonce 1. The noop program must exist on the base layer (deployed on mainnet/devnet; loaded from `test-integration/schedulecommit/elfs/noop.so` in integration configs).

The same uniqueness nonce is appended to every independently signed buffer init, realloc, write, and cleanup transaction sent by `DeliveryPreparator`. Buffer PDAs are keyed by authority, account pubkey, and commit id; because the commit id restarts at 1 after re-delegation, those transactions could otherwise alias a prior delegation instance under the same cached base-layer blockhash. Reusing the intent id keeps retries of one intent idempotent while ensuring a later delegation instance produces distinct signatures. Keep the noop in every buffer lifecycle stage: protecting only initialization still allows an old resize, chunk write, or close status to be mistaken for the current operation.

### Actions and callbacks

Standalone actions are currently built through commit-task paths even when there are no committed accounts. Base actions with callbacks are extracted and scheduled through the `ActionsCallbackScheduler`. `actions_timeout` applies across action-related execution work. If action execution fails with recoverable CPI/limit errors, the executor can strip actions or move from single-stage to two-stage depending on the path; preserve error visibility through `patched_errors` and callback reports.

### Scheduling backpressure

`CommittorProcessor::schedule_intent_bundles` and `execute_intent_bundles` call `IntentScheduleHandle::schedule` directly (no actor/message-channel indirection). Backpressure is handled inside that call: bundles go to the executor's mpsc channel when there is room, otherwise to the `BacklogDB` backlog (see Scheduling and concurrency flow above). `execute_intent_bundles` awaits its oneshot listeners after scheduling, so a caller only returns once every requested intent has broadcast a result.

## Important invariants

1. Do not execute two intent bundles concurrently when their committed-pubkey sets overlap.
2. Preserve FIFO blocking semantics across indirectly blocked intents; later intents must not bypass an earlier blocked intent sharing any key.
3. Do not schedule duplicate intent ids in the same scheduler/execution-extension context.
4. Commit nonces must be fetched with base-layer freshness (`min_context_slot`) and incremented atomically per account.
5. Execution errors must reset cached nonces for all committed pubkeys; successful undelegation must reset only the undelegated pubkeys and preserve other committed-account cache entries.
6. The outbox intent PDA's `ExecutionStage` must be advanced via `OutboxClient::set_intent_execution_stage` before the corresponding base-layer transaction is sent, so a crash after send is still resumable from the recorded stage.
7. Pending-intent recovery must schedule execution for outbox intent PDAs that already exist without re-creating or duplicating them; only `close_intent` on success removes a PDA.
8. Buffer accounts and ALTs must be prepared before transaction assembly uses them, and released/closed only when safe.
9. Failed intent cleanup must not race with retries using the same buffer PDAs; current cleanup is success-only for that reason.
10. Transaction-size and compute-budget choices must keep produced transactions under Solana wire limits.
11. Base-layer sends must preserve explicit processed/committed confirmation semantics from `magicblock-rpc-client`.
12. Intents whose commit uses nonce <= 1 must carry the per-intent uniqueness noop on every stage; otherwise their transactions can alias a prior delegation instance's landed signature and report success without executing.
12. Signer/authority requirements for validator-signed commits, committor-program buffers, ALTs, callbacks, and base-layer instructions must not be relaxed.
13. Outbox intent PDA status/signature updates must continue to expose enough information for diagnostics, retries, and recovery.
14. Avoid adding blocking I/O or unbounded work to the scheduler, executor, task-preparation, or RPC hot paths.

## Common change areas and what to inspect

### Changing service API, startup, or shutdown

Start with `src/service.rs`, `src/committor_processor.rs`, and `magicblock-api/src/magic_validator.rs`. Then inspect `magicblock-api/src/magic_sys_adapter.rs`. Check oneshot dispatcher behavior, channel/backlog capacity, the replica-mode `Disabled` state, cancellation, and whether consumers need errors instead of logged-only failures.

### Changing scheduling or concurrency

Start with `src/intent_engine/intent_scheduler.rs`, `src/intent_engine/intent_execution_engine.rs`, and tests in those files. Verify conflict sets include all committed accounts in mixed bundles, scheduler capacity remains bounded, semaphore permits are always released, and completion cannot corrupt blocked queues.

### Changing commit nonce or metadata fetching

Start with `src/tasks/task_info_fetcher.rs` and `src/tasks/task_builder.rs`. Inspect `magicblock-api/src/magic_sys_adapter.rs` for current nonce queries. Preserve sorted lock acquisition, cache reset behavior, `min_context_slot`, Delegation Program PDA derivation, and retry/error classification.

### Changing task construction or strategy selection

Start with `src/tasks/task_builder.rs`, `src/tasks/task_strategist.rs`, `src/tasks/commit_task.rs`, `src/tasks/commit_finalize_task.rs`, and `src/tasks/utils.rs`. Then inspect `magicblock-committor-program` instruction builders, `magicblock-delegation-program-api` expectations, and integration tests under `test-integration/test-committor-service`. Validate commit ids, allow-undelegation flags, action ordering, diff-vs-state delivery, buffer conversion, ALT keys, and strategy persistence.

### Changing delivery preparation or cleanup

Start with `src/transaction_preparator/mod.rs` and `delivery_preparator.rs`, then inspect `.agents/context/crates/magicblock-committor-program.md`, `.agents/context/crates/magicblock-table-mania.md`, and `.agents/context/crates/magicblock-rpc-client.md`. Check buffer init/realloc/write chunking, retry handling for already-initialized buffers, cached blockhash invalidation, ALT finalized waits, cleanup-on-success only, and release of TableMania refs.

### Changing the outbox intent lifecycle or recovery

Start with `src/outbox/mod.rs`, `src/outbox/outbox_client.rs`, `src/outbox/outbox_intent_bundles_reader.rs`, and `src/service.rs`'s `reschedule_intents`. Also check `programs/magicblock/src/intent_bundles/outbox/` for the on-chain PDA seeds, discriminator, and status-transition validation, and `magicblock-core/src/intent/outbox.rs` for `outbox_intent_pda`/`OUTBOX_INTENT_DISCRIMINATOR`. Preserve PDA seed/discriminator compatibility, `OutboxIntentBundleStatus` transition validity, and the invariant that closing an outbox intent PDA must not be gated on `CoordinationMode` — replicas replaying a primary's close have to reach the same state, or the PDA leaks and the intent can be re-executed.

### Changing metrics or observability

Start with metric calls in `intent_execution_engine.rs`, `delivery_preparator.rs`, and `intent_execution_client.rs`. Keep this guide focused on local instrumentation intent; metric naming, labels, and registry details belong in `.agents/context/crates/magicblock-metrics.md`.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-committor-service`.
- Relevant integration suites: `test-committor`, including preparators, ix-order, ix-multi, commit-finalize, intent-executor, and recovery targets; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Related suite intent: when TableMania or RPC-client behavior is touched, include the TableMania suite or focused committor preparation/delivery coverage.
- Performance/security validation intent: report effects on executor parallelism, RPC calls, transaction count, ALT waits, buffer writes/chunks, outbox intent PDA writes/closes, and cleanup latency; confirm signer/authority requirements, `min_context_slot` freshness, nonce sequencing, scheduler conflict blocking, and recovery durability remain intact.


## Adjacent implementation references

- `.agents/context/crates/magicblock-committor-program.md` — buffer/chunks on-chain helper contracts.
- `.agents/context/crates/magicblock-rpc-client.md` — base-layer send/confirm and RPC helper behavior.
- `.agents/context/crates/magicblock-table-mania.md` — ALT lifecycle and finalized-read semantics.
- `.agents/context/crates/magicblock-services.md` — request-driven local scheduling that can create commit-and-undelegate intents.
- `magicblock-committor-service/README.md` — high-level implementation notes.
- `test-integration/test-committor-service/` — integration coverage of delivery and intent execution.
