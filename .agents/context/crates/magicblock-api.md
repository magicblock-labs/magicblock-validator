# `magicblock-api`

## Purpose

`magicblock-api` is the validator orchestration crate. It turns `magicblock-config::ValidatorParams` into a running validator service graph, owns the `MagicValidator` lifecycle, and wires together storage, account synchronization, RPC/pubsub, transaction execution, settlement, replication, metrics, task scheduling, and operator-facing on-chain registration flows.

High-level responsibilities:

- initialize persistent ledger and AccountsDb state;
- sync and verify the validator keypair stored beside the ledger;
- build genesis/sys accounts required by the local runtime;
- create Chainlink/account-cloning, committor, DLP undelegation request, replication, metrics, task-scheduler, and Aperture RPC services;
- spawn transaction execution, RPC runtime, slot/system tickers, ledger truncation, and optional replication service threads/tasks;
- transition the scheduler out of `StartingUp` after replay/reset/recovery;
- register/unregister the validator in the Magic Domain Program and manage validator fee-vault setup in standalone ephemeral mode;
- stop services in an order that protects in-flight commits and flushes durable state.

This crate sits directly on startup and shutdown paths and coordinates hot-path services owned by other crates. Changes here can introduce latency, ordering bugs, persistence/recovery failures, or primary/replica divergence even when the changed code is not itself in an execution hot loop.

## Update requirement

Update this document in the same change whenever `magicblock-api` behavior or contracts change. Update it for changes to:

- `MagicValidator::try_from_config`, `start`, `stop`, `prepare_ledger_for_shutdown`, or validator registration/unregistration behavior;
- startup/shutdown ordering, service ownership, cancellation semantics, or thread/runtime spawning;
- ledger, AccountsDb, keypair, genesis, MagicContext, ephemeral vault, or native mint initialization;
- Chainlink, committor, DLP undelegation request processing, replication, task scheduler, RPC/Aperture, metrics, or ledger truncator wiring;
- scheduler mode transitions for standalone, primary, or replica roles;
- MagicSys/commit nonce lookup behavior;
- domain registry, fee vault, claim-fees, or other base-layer operator flows;
- public APIs, exported modules, error types, validation commands, or integration-test expectations.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-api/src/lib.rs` | Public module exports: `domain_registry_manager`, `errors`, `ledger`, and `magic_validator`; internal wiring modules stay private. |
| `magicblock-api/src/magic_validator.rs` | Main `MagicValidator` implementation and startup/shutdown service graph. |
| `magicblock-api/src/domain_registry_manager.rs` | Magic Domain Program registration, sync, fetch, and unregister helpers. |
| `magicblock-api/src/ledger.rs` | Ledger open/reset helpers, ledger lockfile helpers used by the binary, and validator-keypair persistence beside the ledger. |
| `magicblock-api/src/fund_account.rs` | Local genesis/runtime account seeding for validator identity, `MagicContext`, and ephemeral vault. |
| `magicblock-api/src/genesis_utils.rs` | Local genesis account construction, native mint setup, and feature activation helpers. |
| `magicblock-api/src/magic_sys_adapter.rs` | Adapter from Magic Program `MagicSys` trait calls to `CommittorService` commit-nonce lookups. |
| `magicblock-api/src/tickers.rs` | Slot ticker for accepting scheduled commits and system metrics ticker for storage/account gauges. |
| `magicblock-api/src/errors.rs` | `ApiError` and `ApiResult`, including boxed conversions from downstream service errors. |
| `magicblock-api/README.md` | Short public usage example for constructing and starting `MagicValidator`. |
| `magicblock-validator/src/main.rs` | Primary consumer: parses config, creates `MagicValidator`, takes the ledger lock, starts/stops the validator, and drives TUI/headless lifecycle. |
| `test-integration/test-magicblock-api/` | Integration coverage for domain registry, claim fees, and block/transaction timestamp stability. |
| `config.example.toml` | Operator-facing configuration consumed through `ValidatorParams` and wired by this crate. |

Main consumers:

- `magicblock-validator` is the only production binary consumer of `MagicValidator`.
- `test-integration/test-magicblock-api` directly uses `DomainRegistryManager` and exercises runtime behavior through integration contexts.
- Other crates are mostly downstream services wired by this crate rather than consumers of it.

Important downstream dependencies include `magicblock-accounts-db`, `magicblock-ledger`, `magicblock-processor`, `magicblock-aperture`, `magicblock-chainlink`, `magicblock-account-cloner`, `magicblock-committor-service`, `magicblock-replicator`, `magicblock-task-scheduler`, `magicblock-metrics`, `magicblock-services`, `magicblock-program`, and `magicblock-validator-admin`.

## Public API shape / Main public types and APIs

The exported public API is intentionally small for production use:

- `magic_validator::MagicValidator`
  - `try_from_config(ValidatorParams) -> ApiResult<MagicValidator>` builds the service graph and spawns some long-lived components such as transaction execution and the RPC runtime.
  - `start(&mut self) -> ApiResult<()>` performs ledger replay/reset/recovery, switches scheduler/replication mode, and starts post-start services such as slot ticking, ledger truncation, claim-fees, and the task scheduler.
  - `stop(self)` consumes the validator and shuts down services, joins threads/tasks where available, flushes AccountsDb and ledger, and performs ledger shutdown.
  - `start_unregister_validator_on_chain(&mut self)` sends a best-effort unregister transaction for standalone ephemeral validators that use on-chain coordination.
  - `prepare_ledger_for_shutdown(&mut self)` stops truncation, cancels manual compactions, and flushes the ledger before final shutdown.
  - `ledger(&self) -> &Ledger` exposes the ledger for the binary to lock and report paths.
- `domain_registry_manager::DomainRegistryManager`
  - builds Solana RPC clients for the Magic Domain Program;
  - fetches `ErRecord` data;
  - registers, syncs, and unregisters validator records;
  - has static helper variants used by `MagicValidator` background startup/shutdown flows.
- `ledger`
  - `ledger_lockfile` and `lock_ledger` are public for the binary to prevent multiple validators using the same ledger path.
  - validator-keypair and reset/open helpers are crate-private.
- `errors::{ApiError, ApiResult}` is the crate-level error surface.

Private but important internal APIs:

- `MagicSysAdapter` implements `magicblock_core::traits::MagicSys` so Magic Program execution can synchronously ask the committor service for current commit nonces.
- `init_intent_execution_service` wires the committor service to periodically accept scheduled intents from ER state and process results back through local validator transactions.
- `init_system_metrics_ticker` periodically updates storage/account gauges.

## Runtime flows

### Construction flow: `MagicValidator::try_from_config`

```text
ValidatorParams
  -> ledger open/reset + keypair sync
  -> AccountsDb open/snapshot/genesis/sys accounts
  -> committor + MagicSys adapter
  -> chainlink/account cloner
  -> replication service (optional)
  -> metrics + system metrics ticker
  -> intent execution service + undelegation request service
  -> program loading + validator authority init
  -> transaction scheduler/execution thread
  -> Aperture RPC runtime thread
  -> task scheduler construction
```

Current order matters:

1. Create a cancellation token and clone the configured validator keypair.
2. Build local genesis accounts from the validator pubkey and base fee.
3. Open/reset the ledger, derive `last_slot`, and sync the validator keypair file stored beside the ledger.
4. Open AccountsDb at the ledger-derived slot.
5. Connect to a replication broker for primary/replica modes; a fresh replica may import an external snapshot and set `config.accountsdb.reset = true` so Chainlink does not prune replica state.
6. Create the committor service and install `MagicSysAdapter` with `init_magic_sys`.
7. Create Chainlink unless the role is `ReplicationMode::Replica`, where `ChainlinkImpl::disabled()` is used.
8. Create replication service if a broker was configured.
9. Insert missing genesis accounts, initialize validator identity, MagicContext, and ephemeral vault accounts.
10. Start metrics service and system metrics ticker.
11. Create the DLP undelegation request service for non-replica validators.
12. Load configured upgradeable programs and initialize Magic Program validator authority/override.
13. Build the SVM environment, transaction scheduler state, and executor count; spawn the transaction execution thread.
14. Build Aperture shared state and start the RPC server on its own Tokio runtime thread.
15. Construct the task scheduler database path next to the ledger parent and create `TaskSchedulerService`.

Caveats:

- `try_from_config` already spawns long-lived execution and RPC resources before `start()` is called.
- Startup timing is logged via `log_timing`; preserve useful timings if changing slow startup sections.
- The RPC runtime uses roughly half available CPUs minus one worker, while the process main runtime is created in `magicblock-validator`.

### Start flow: replay, reset, mode transition, recovery

1. `maybe_process_ledger` skips replay when `ledger.reset` is set or AccountsDb slot is already at least the ledger slot.
2. Ledger replay uses `process_ledger` with a blockhash age derived from configured block time.
3. After replay, Magic Program scheduled actions are cleared so replayed accept-commit transactions do not re-commit.
4. Optional AccountsDb defragmentation runs before normal work starts; this is explicitly marked safe only before cleanup, scheduler mode changes, replication, slot ticks, or task recovery.
5. Standalone/primary nodes reset the bank and, for primaries, send a replication `Message::Reset`.
6. Pending commit intent recovery runs only after replay and reset.
7. Standalone nodes switch the scheduler to `SchedulerMode::Primary`; primary/replica modes spawn the replication service instead.
8. Non-replica validators start DLP undelegation request subscription/backfill through `UndelegationRequestService::start`; replicas keep Chainlink disabled and do not scan DLP requests.
9. Standalone ephemeral nodes start background base-layer setup: funding check, magic fee vault init/delegation, optional startup fee claim, and optional domain registration.
10. Claim-fees periodic task, slot ticker, ledger truncator, and primary-only task scheduler are started.

### Scheduled intent execution flow

```text
intent execution service sleep
  -> read MagicContext from AccountsDb
  -> if scheduled commits exist, execute AcceptScheduleCommits tx through scheduler
  -> committor service schedules base-layer work
  -> result processor submits ScheduledCommitSent locally
```

The service uses the same transaction scheduler path as normal execution for validator-signed local transactions such as `AcceptScheduleCommits` and `ScheduledCommitSent`. Keep this work outside RPC request handling and transaction-execution hot loops.

### Shutdown flow: `MagicValidator::stop`

1. Send unregister transaction in the background when standalone ephemeral on-chain coordination is enabled.
2. Set the local `exit` flag and cancel the shared cancellation token.
3. Stop the DLP undelegation request service, then the intent execution service. Treat this ordering as compatibility-sensitive and inspect in-flight intent behavior before changing it.
4. Stop claim-fees task.
5. Join RPC thread, slot ticker, ledger truncator, replication thread, and transaction execution thread.
6. Flush AccountsDb.
7. Flush and shut down ledger.
8. Join unregister confirmation thread only if it has already finished; shutdown does not wait for that confirmation.

Durable state is flushed only after workers that can admit, commit, truncate, or replicate state are stopped.

### Domain registry and base-layer operator flow

Standalone ephemeral validators that need on-chain interactions may:

1. verify the validator authority has at least 5 SOL on the base layer;
2. initialize and delegate the validator magic fee vault if missing;
3. optionally claim existing validator fees on startup and periodically thereafter;
4. register or sync an `ErRecord` in the Magic Domain Program;
5. send an unregister transaction during shutdown and confirm it in a background thread with an 8s timeout and 400ms polling interval.

The domain registry manager sends ordinary Solana transactions against the configured base-layer RPC URL. Changes here affect operator-facing registration semantics and should be tested against the `test-magicblock-api` integration suite when possible.

## Important internals and caveats

### Startup/shutdown ordering is the main contract

`magicblock-api` is mostly glue, but the glue order is correctness-critical. Ledger replay must finish before account reset and scheduler mode transition. Pending intent recovery must run after replay/reset. Replica startup must not let Chainlink prune state imported from the primary. Shutdown must prevent more state changes before flushing storage.

### Replication modes intentionally diverge

`ReplicationMode::Standalone` and `ReplicationMode::Primary` use enabled Chainlink. `ReplicationMode::Replica` uses disabled Chainlink and waits for replicated state. The unit tests in `magic_validator.rs` cover this helper. Do not “simplify” the modes into one startup path without checking replication service expectations.

### `MagicSysAdapter` blocks on a synchronous committor response

Magic Program execution can call `fetch_current_commit_nonces`; the adapter converts this into a committor-service sync-channel request and waits up to 30 seconds. This is not a place to add unbounded blocking, and error codes are surfaced as `InstructionError::Custom` values.

### Local sys accounts are special

`fund_account.rs` seeds the validator identity as privileged, `MagicContext` as delegated and Magic Program-owned, and the ephemeral vault as ephemeral and Magic Program-owned. Those flags are part of execution/access-control behavior and must stay aligned with Magic Program/SVM invariants.

### Ledger keypair verification protects operators

The ledger stores a validator keypair file beside the blockstore parent. With `verify_keypair` enabled, a mismatch between the configured keypair and stored ledger keypair fails startup. Do not bypass this without understanding operator safety and persisted-state identity assumptions.

## Important invariants

1. Ledger replay, optional defragmentation, bank reset, pending intent recovery, and scheduler mode transition must remain ordered so execution does not race recovery or cleanup.
2. Replicas must not use enabled Chainlink or local cleanup in ways that diverge from primary-replicated state.
3. Scheduled commit recovery must run only after replay and reset, because it reads local account state for delegation checks.
4. `MagicContext` must exist before the slot ticker can run, and it must remain Magic Program-owned and delegated locally.
5. Ephemeral vault initialization must preserve the ephemeral flag and Magic Program owner.
6. Validator identity must remain privileged in local AccountsDb.
7. Committor service wiring must stay available before Magic Program execution can fetch commit nonces or schedule settlement work.
8. DLP undelegation request polling must stay in the background service path, not the slot ticker or transaction-execution hot path.
9. Do not add blocking RPC, slow I/O, or expensive serialization to scheduler/executor hot paths from this crate; keep such work in startup/background services where possible.
10. Shutdown must cancel/stop services before flushing AccountsDb and ledger.
11. Operator-facing config keys and base-layer registration/fee-vault behavior are compatibility-sensitive.

## Common change areas and what to inspect

### Changing validator startup or service wiring

Inspect first:

- `magicblock-api/src/magic_validator.rs` around `try_from_config` and `start`;
- downstream service constructors (`initialize_aperture`, `CommittorService::try_start`, `ProdInnerChainlink::try_new_from_endpoints`, `TransactionScheduler::new`, `TaskSchedulerService::new`);
- `.agents/context/architecture.md` startup path and service boundaries;
- `magicblock-validator/src/main.rs` for main runtime, ledger lock, TUI/headless lifecycle.

Risks:

- starting RPC or task scheduler before replay/reset/recovery is complete;
- double-spawning services that are currently constructed in `try_from_config` but started in `start`;
- breaking primary/replica mode transitions.

### Changing shutdown behavior

Inspect first:

- `MagicValidator::stop`;
- `prepare_ledger_for_shutdown` and `magicblock-validator/src/main.rs` shutdown sequence;
- downstream service `stop`/`join` semantics for scheduled commits, committor, ledger truncator, replication, transaction scheduler, task scheduler, and RPC.

Risks:

- flushing state while workers can still mutate it;
- dropping Tokio runtimes before required async cleanup;
- waiting indefinitely for unregister confirmation or background services.

### Changing commit, fee vault, or domain registration flows

Inspect first:

- `magicblock-api/src/domain_registry_manager.rs`;
- `spawn_primary_onchain_setup`, `ensure_magic_fee_vault_on_chain`, `register_validator_on_chain`, and `start_unregister_validator_on_chain`;
- `magicblock-validator-admin/src/claim_fees.rs`;
- Magic Domain Program and Delegation Program instruction builders;
- `test-integration/test-magicblock-api/tests/test_domain_registry.rs` and `test_claim_fees.rs`.

Risks:

- base-layer RPC commitment or confirmation changes affecting operator startup;
- fee vault not being delegated before commit scheduling;
- registering the wrong authority when replication authority override is active.

### Changing local account/genesis setup

Inspect first:

- `fund_account.rs`;
- `genesis_utils.rs`;
- Magic Program API constants for `MAGIC_CONTEXT_PUBKEY` and `EPHEMERAL_VAULT_PUBKEY`;
- SVM/account access validation docs in `.agents/specs/validator-specification.md`.

Risks:

- missing local sys accounts causing startup or scheduled-commit ticker panics;
- changing account flags that SVM access validation relies on;
- primary/replica genesis state divergence.

### Changing validation, replay, or timestamps

Inspect first:

- `maybe_process_ledger`;
- `magicblock-ledger::blockstore_processor::process_ledger`;
- `test-integration/test-magicblock-api/tests/test_clocks_match.rs`;
- `test-integration/test-magicblock-api/tests/test_get_block_timestamp_stability.rs`.

Risks:

- replaying scheduled commit accept transactions in a way that re-commits;
- inconsistent block time across transaction status, ledger block, and RPC block-time APIs.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-api`.
- Relevant integration suites: `test-magicblock-api`; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Related validation intent: startup/shutdown changes should report timing-sensitive effects and preserve `log_timing` coverage where useful; changes that move work into RPC, account sync, scheduler/executor, ledger, replication, or committor paths need focused performance reasoning or measurement.

## Adjacent implementation references

- `.agents/context/crates/magicblock-aperture.md` — RPC/pubsub service details wired by this crate.
- `.agents/context/crates/magicblock-account-cloner.md`, `.agents/context/crates/magicblock-chainlink.md`, `.agents/context/crates/magicblock-services.md`, and `.agents/context/crates/magicblock-committor-service.md` — account sync, DLP request, and scheduled-intent dependencies wired by this crate.
- `magicblock-api/README.md` — short usage example for `MagicValidator`.
- `magicblock-validator/src/main.rs` — production binary lifecycle around `MagicValidator`.
- `test-integration/test-magicblock-api/` — integration tests for API-owned flows.
