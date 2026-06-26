# `test-kit`

## Purpose

`test-kit/` is the shared Rust test-support crate for validator unit tests, crate tests, and integration-test crates. It provides a lightweight in-process validator execution harness plus common logging and Solana convenience re-exports so tests can exercise scheduler, SVM execution, AccountsDb, Ledger, Magic Program, and RPC-adjacent behavior without duplicating setup code.

High-level responsibilities:

- create temporary `AccountsDb` and `Ledger` instances for tests;
- wire `magicblock-core::link` channels into a `magicblock-processor::TransactionScheduler`;
- build an SVM environment, load the `guinea` test program, fund test payers, and expose helpers for account setup;
- submit transactions through execute, schedule, simulate, and replay paths;
- expose test logging macros and devnet-availability skip helpers;
- re-export common Solana instruction/signing types and the `guinea` test program for concise tests.

This crate is not production runtime code, but it wraps performance- and correctness-sensitive runtime crates. Changes here can hide or create false failures in scheduler/execution, ledger, Magic Program, RPC, and integration suites. Keep helpers faithful to production semantics unless a test-only shortcut is deliberate and documented in this guide.

## Update requirement

Update this guide in the same change whenever `test-kit` behavior or contracts change. In particular, update it for changes to:

- `ExecutionTestEnv` construction, scheduler startup/mode handling, shutdown behavior, default fee, block time, or superblock sizing;
- helper semantics that mark accounts delegated, fund payers, advance slots, load programs, or write directly to `AccountsDb`/`Ledger`;
- public helper methods, re-exports, macros, or logging environment variables;
- the required location or build assumptions for `../programs/elfs/guinea.so`;
- validation commands or important consumer suites that should be run after harness changes.

Also update this file if another crate changes an API that alters how this harness wires runtime services, such as processor scheduler handles, ledger block APIs, account flags, or core channel endpoints.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

| Path | Role |
|---|---|
| `test-kit/Cargo.toml` | Package manifest. Depends on `guinea`, `magicblock-accounts-db`, `magicblock-core`, `magicblock-ledger`, `magicblock-processor`, Solana transaction/instruction/signing crates, `tempfile`, `tokio`, and tracing/logging crates. |
| `test-kit/src/lib.rs` | Main public API. Defines `ExecutionTestEnv`, `CommitableAccount`, constants, transaction/account helpers, scheduler lifecycle helpers, and public re-exports. |
| `test-kit/src/macros.rs` | Logging/devnet helpers plus exported `init_logger!` and `skip_if_devnet_down!` macros. |
| `programs/guinea/` | Test-only program re-exported as `test_kit::guinea` and loaded into `ExecutionTestEnv`. |
| `programs/elfs/guinea.so` | Runtime artifact loaded by `ExecutionTestEnv` using relative path `../programs/elfs/guinea.so`. Tests that instantiate the harness require this ELF to exist at that path relative to the test process working directory. |
| `magicblock-processor/tests/` | Main direct consumer of `ExecutionTestEnv` for execution, scheduling, replay, replica ordering, simulation, fees, security, and ephemeral-account tests. |
| `magicblock-aperture/tests/` | Builds RPC test environments around `ExecutionTestEnv` and uses re-exported instruction/signing conveniences. |
| `magicblock-ledger/src/**` and `magicblock-ledger/tests/` | Use `init_logger!` for ledger unit/integration tests. |
| `programs/magicblock/src/**` | Uses `init_logger!` in Magic Program unit tests. |
| `test-integration/**` | Uses `init_logger!`, `Signer`, `Instruction`, `AccountMeta`, and `guinea` across cloning, committor, config, restore-ledger, schedule-intent, table-mania, and MagicBlock API suites. |

Main consumers:

- `magicblock-processor` tests use the full execution harness most heavily.
- `magicblock-aperture` tests embed `ExecutionTestEnv` behind live JSON-RPC/pubsub servers.
- `programs/magicblock`, `magicblock-ledger`, `magicblock-committor-service`, and integration crates mostly use logging macros and re-exports.

Important upstream dependencies:

- `magicblock-core::link` channel types and `TransactionSchedulerHandle` define the harness boundary for transaction submission and event observation.
- `magicblock-processor::{build_svm_env, TransactionScheduler, TransactionSchedulerState}` define execution semantics.
- `magicblock-accounts-db` and `magicblock-ledger` provide temporary persistence state.
- Solana account, instruction, keypair, signer, transaction, and status types define test API compatibility.

## Public API shape / Main public types and APIs

Public re-exports from `src/lib.rs`:

- `pub use guinea;` exposes `test_kit::guinea::{ID, GuineaInstruction, ...}`.
- `pub use solana_instruction::*;` exposes `Instruction`, `AccountMeta`, and related instruction types.
- `pub use solana_signer::Signer;` keeps tests concise when calling `.pubkey()` on keypairs.
- `pub mod macros;` exposes logging/devnet helper functions in addition to exported macros.

Key constants:

| Item | Meaning |
|---|---|
| `ExecutionTestEnv::BASE_FEE` | Default base fee, currently `1000` lamports. |
| `BLOCK_TIME` | Scheduler block time used by the harness, currently `50ms`. |
| `SUPERBLOCK_SIZE` | Default superblock interval, currently `72000`. |

### `ExecutionTestEnv`

`ExecutionTestEnv` owns an in-process execution stack for tests:

- `payers: Vec<Keypair>` — one generated payer per configured executor.
- `accountsdb: Arc<AccountsDb>` and `ledger: Arc<Ledger>` — temporary persistent state.
- `transaction_scheduler: TransactionSchedulerHandle` — async submission API exposed through core dispatch channels.
- `dispatch: DispatchEndpoints` — event/submission endpoints for tests that need account/status streams.
- `scheduler: Option<TransactionScheduler>` and `run_scheduler()` — deferred-start support for tests that need to enqueue work before launching the scheduler.
- `shutdown: CancellationToken` and `Drop` — cancels and joins the scheduler thread on environment drop.

Important constructors:

| Constructor | Use |
|---|---|
| `ExecutionTestEnv::new()` | Default primary-mode environment with `BASE_FEE`, one executor, and immediate scheduler startup. |
| `ExecutionTestEnv::new_with_config(fee, executors, defer_startup)` | Primary-mode environment with custom fee/executor count and optional deferred scheduler startup. |
| `ExecutionTestEnv::new_replica_mode(executors, defer_startup)` | Replica-mode environment for replay-ordering tests; does not pre-send `Primary` mode. |
| `ExecutionTestEnv::new_replica_mode_with_superblock_size(executors, defer_startup, superblock_size)` | Replica-mode variant with custom superblock interval. |

Important methods:

- Scheduler/mode helpers: `run_scheduler`, `switch_to_primary_mode`, `wait_for_scheduler_ready`, `yield_to_scheduler`, `wait_for_next_slot`.
- Slot helper: `advance_slot` writes a new `LatestBlockInner`, updates AccountsDb slot, and yields the current thread.
- Account helpers: `create_account_with_config`, `create_account`, `fund_account`, `fund_account_with_owner`, `get_account`, `try_get_account`, `get_payer`.
- Transaction helpers: `build_transaction`, `build_transaction_with_signers`, `execute_transaction`, `schedule_transaction`, `simulate_transaction`, `replay_transaction`, `get_transaction`.

### `CommitableAccount`

`CommitableAccount<'db>` is a mutable account snapshot returned by `get_account`, `try_get_account`, and `get_payer`:

- it dereferences to `AccountSharedData` and supports mutable account edits;
- changes are local to the wrapper until `commit()` reinserts the account into `AccountsDb`;
- dropping without `commit()` discards modifications.

### Macros and logging helpers

`test-kit/src/macros.rs` provides:

- `init_logger_for_tests()` — initializes `LogTracer` and a tracing subscriber, respecting `RUST_LOG`; if `RUST_LOG_STYLE=test` and `TEST_FILE_PATH` is set, it appends the test file stem at debug level.
- `init_logger_for_test_path(full_path_to_test_file)` — legacy path-aware logger used by `init_logger!`; if `RUST_LOG` ends with `,` or is `info`, it appends `<test_file_stem>=<RUST_LOG_LEVEL or info>`.
- `init_logger!()` — exported macro that passes `std::file!()` to `init_logger_for_test_path`.
- `is_devnet_up().await` — checks `https://api.devnet.solana.com` with a nonblocking RPC client.
- `skip_if_devnet_down!()` — exported async-test macro that logs a warning and returns early when devnet is unavailable.

Logger initialization intentionally ignores repeated-initialization errors so tests can call it freely.

## Runtime flows

### Primary-mode execution harness setup

1. `ExecutionTestEnv::new()` calls `new_with_config(BASE_FEE, 1, false)`.
2. Construction initializes tracing through `init_logger!()`.
3. A temporary directory is created and used to open both `AccountsDb` and `Ledger`.
4. `magicblock_core::link::link()` creates dispatch endpoints and validator channels.
5. The current ledger blockhash seeds `build_svm_env(&accountsdb, blockhash, fee)`.
6. One payer keypair is generated per configured executor.
7. A mode channel is created and `SchedulerMode::Primary` is pre-sent for primary-mode constructors.
8. The harness advances to slot `1` before loading programs.
9. `load_upgradeable_programs` loads `(guinea::ID, "../programs/elfs/guinea.so")` into AccountsDb.
10. `TransactionSchedulerState` is assembled from AccountsDb, Ledger, channel receivers/senders, SVM environment, feature set, shutdown token, mode receiver, pause permit, block time, and superblock size.
11. A `TransactionScheduler` is created. It is spawned immediately unless `defer_startup` is true.
12. Each payer is funded with `LAMPORTS_PER_SOL` through `fund_account`, which creates delegated system accounts.

Pitfalls:

- The relative guinea ELF path is part of the current test harness contract. If tests run from a different working directory or before SBF artifacts are built, harness construction can fail.
- `payers` length equals the configured executor count. Most tests use at least one executor; adding zero-executor use requires auditing payer indexing and scheduler behavior.

### Deferred scheduler flow

1. Construct with `defer_startup = true`; the scheduler is stored in `ExecutionTestEnv::scheduler` and no scheduler thread is running.
2. Tests may enqueue transactions through `schedule_transaction` or inspect channels before execution begins.
3. `run_scheduler()` takes the stored scheduler and spawns it, storing the join handle.
4. Calling `run_scheduler()` again after the scheduler has been taken is a no-op.

Use this only for tests that intentionally need deterministic pre-start scheduling. For ordinary execution tests, prefer immediate startup.

### Replica and replay flow

1. `new_replica_mode` constructs the same storage/channel stack but does not pre-send `SchedulerMode::Primary`.
2. The scheduler starts in replica-oriented behavior and accepts replay submissions via `replay_transaction`.
3. `replay_transaction(persist, txn)` submits a `ReplayPosition { slot: 0, index: 0, persist }`.
4. `switch_to_primary_mode()` can later send `SchedulerMode::Primary` when a test needs a transition.

Replica-mode tests depend on ordering and persistence semantics; avoid changing replay defaults without updating `magicblock-processor/tests/replay.rs` and `magicblock-processor/tests/replica_ordering.rs`.

### Account and transaction helper flow

```text
test creates/funds account
  -> helper writes AccountSharedData directly into AccountsDb
  -> helper marks many created/funded accounts delegated
  -> test builds transaction with current ledger latest_blockhash
  -> TransactionSchedulerHandle execute/simulate/schedule/replay path
  -> processor updates AccountsDb/Ledger/events
  -> test reads status/account helpers
```

Direct AccountsDb writes are a test shortcut. They bypass cloning, delegation-record fetching, and RPC/account-sync behavior. Use integration tests with live validators when the behavior under test depends on those layers.

## Important internals and caveats

### Delegated-by-default account helpers

`create_account_with_config` and `fund_account_with_owner` call `account.set_delegated(true)` before inserting into AccountsDb; `create_account` and `fund_account` inherit that behavior. This makes local execution helpers convenient because MagicBlock SVM access validation allows delegated writable accounts. If a test needs an undelegated or confined account, it must fetch the account with `get_account`, mutate flags/owner/data, and `commit()` the change.

### Payer rotation

`build_transaction` and `build_transaction_with_signers` use an atomic counter and rotate across `payers[index % payers.len()]`. This lets multi-executor tests avoid a single payer becoming a universal write-lock bottleneck. Preserve this behavior when changing transaction builders unless the affected scheduling tests are updated deliberately.

### Commitable account snapshots

`get_account` clones the current account into a `CommitableAccount`; mutations do not affect AccountsDb until `commit()`. This pattern is easy to misuse in tests: always call `commit()` after changing lamports, data, owner, or account flags.

### Scheduler readiness waits

`wait_for_scheduler_ready` waits for the latest block slot to advance and times out after five seconds. `yield_to_scheduler` only yields and sleeps briefly. Choose the stronger readiness helper when a test depends on the scheduler processing mode changes or slot ticks; use the lighter helper for replay tests that should not require slot advancement.

### Logger initialization

Both logger helpers ignore repeated initialization failures from `LogTracer` and tracing subscriber setup. This is intentional for parallel test binaries. Do not replace it with panicking initialization unless every caller is audited.

### Devnet skip helper

`skip_if_devnet_down!()` performs a real network call to public Solana devnet and returns from the current async test when unavailable. Keep this out of unit tests and deterministic local tests; prefer it only for tests that already require devnet.

## Important invariants

1. `test-kit` must remain test-only support; do not make production runtime crates depend on it outside dev/test contexts.
2. `ExecutionTestEnv` must cancel the scheduler and join its thread on drop to avoid leaking background workers between tests.
3. The harness must wire scheduler channels consistently with `magicblock-core::link`; otherwise tests may pass while production orchestration differs.
4. Account helpers that currently create delegated accounts must keep that behavior or all consumers relying on writable local execution must be updated.
5. Transaction builders must sign with the selected payer and use `ledger.latest_blockhash()` so tests exercise current blockhash behavior.
6. `simulate_transaction` must not persist account or ledger side effects; consumers use it to verify simulation isolation.
7. Replica-mode constructors must not silently switch to primary mode; replay-ordering tests rely on the mode distinction.
8. `CommitableAccount` mutations must remain explicit via `commit()`; implicit write-back on drop would change many tests' semantics.
9. Logger macros must stay safe to call multiple times in one test process.
10. The guinea program ID and loaded ELF must stay aligned with `test_kit::guinea`; stale ELF paths or mismatched program IDs make harness results misleading.

## Common change areas and what to inspect

### Change execution harness startup or scheduler wiring

Start with:

- `test-kit/src/lib.rs` constructors and `TransactionSchedulerState` assembly;
- `magicblock-core/src/link/**` for channel/API changes;
- `magicblock-processor/src/scheduler/**` for scheduler state and mode handling;
- `magicblock-processor/tests/scheduling.rs`, `replay.rs`, and `replica_ordering.rs`.

Check deferred startup, primary mode pre-send, replica mode, shutdown, and event channel behavior.

### Change account setup helpers or default flags

Start with:

- `create_account_with_config`, `fund_account_with_owner`, `get_account`, `try_get_account`, and `CommitableAccount::commit`;
- `magicblock-processor/tests/security.rs`, `fees.rs`, `ephemeral_accounts.rs`, and `execution.rs`;
- MagicBlock SVM access-validation rules in `.agents/specs/validator-specification.md`.

Be explicit about whether helper-created accounts should be delegated, undelegated, confined, ephemeral, or system-owned.

### Change transaction builders or payer behavior

Start with:

- `build_transaction`, `build_transaction_with_signers`, and `payer_index` handling;
- scheduling tests that depend on avoiding payer lock contention;
- fee tests that inspect payer balances and gasless mode.

Preserve payer rotation unless deliberately changing lock-conflict behavior in tests.

### Change logging macros or devnet helpers

Start with:

- `test-kit/src/macros.rs`;
- `magicblock-ledger` and `programs/magicblock` unit tests using `init_logger!`;
- `test-integration/**` tests using `init_logger!` or devnet skips.

Keep repeated initialization non-fatal and document any environment-variable changes.

### Change guinea loading or re-exports

Start with:

- `test-kit/src/lib.rs` `pub use guinea` and `load_upgradeable_programs` call;
- `programs/guinea/` and generated/build artifacts under `programs/elfs/`;
- processor, aperture, and integration tests importing `test_kit::guinea`, `Instruction`, `AccountMeta`, or `Signer`.

Validate from the same working directory used by CI/test commands so the relative ELF path is exercised.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `test-kit` plus the smallest affected consumer suite because `test-kit` has little standalone coverage.
- Common consumer packages: `magicblock-processor`, `magicblock-ledger`, `magicblock-aperture`, and `magicblock-committor-service`.
- Relevant integration suites for helper/logging changes include `test-cloning`, `test-magicblock-api`, `test-restore-ledger`, and `test-schedule-intents`; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Performance/concurrency risk to report for harness changes: scheduler configuration, executor count, payer rotation, block timing, channel wiring, and direct storage setup.

## Adjacent implementation references

- For harness boundaries, see `.agents/context/crates/magicblock-core.md` for channel and shared-type ownership.
- Protocol/task consumers are covered by `.agents/context/crates/magicblock-magic-program-api.md` and `.agents/context/crates/magicblock-task-scheduler.md`.
- Representative full-harness call sites live in `magicblock-processor/tests/` and `magicblock-aperture/tests/setup.rs`.
- Test program artifacts are maintained in `programs/guinea/` and `programs/elfs/guinea.so`.
- Use `test-integration/**` for integration crates that exercise logging macros and Solana re-exports.
