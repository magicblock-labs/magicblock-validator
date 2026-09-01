# `magicblock-task-scheduler`

## Purpose

`magicblock-task-scheduler` is the leader-side service that turns Magic Program
scheduled-task side effects into Hydra ephemeral crank create/cancel
transactions. Programs schedule or cancel tasks during normal ER execution;
the Magic Program enqueues a `TaskRequest` through `nucleus::tls::TlsManager`;
the engine publishes that payload as a service message after the originating
transaction commits; this crate deserializes the request and submits a
validator-sponsored Hydra transaction through the local Aperture HTTP
endpoint.

This crate does **not** persist tasks, delay them, retry them, or execute
them. Recurring execution is owned by the ephemeral Hydra program once the
crank account exists. The service is stateless: each crank PDA is derived
from `(authority, task_id)`, so cancel and reschedule need no database
lookup.

High-level responsibilities:

- subscribe to engine service messages and ignore anything that is not a
  `TaskRequest`;
- create and fund a Hydra crank for `Schedule` requests;
- close a Hydra crank for `Cancel` requests, refunding remaining lamports to
  the validator identity;
- replace an existing crank on reschedule by cancelling then recreating it in
  one transaction;
- convert millisecond intervals into Hydra's slot-based cadence using the
  validator blocktime.

This crate sits on the schedule/cancel path and can affect request latency by
how quickly it drains service messages and how much local RPC work it
performs. It is not persistence-sensitive: restart recovery is Hydra account
state, not a local SQLite file.

## Update requirement

Queue an update to this guide for the weekly documentation-maintenance task
whenever behavior or contracts in `magicblock-task-scheduler` change. Include
changes to:

- public exports in `src/lib.rs`, `TaskSchedulerService`, `crank_pubkey`, or
  `TaskSchedulerError`;
- crank PDA derivation, Hydra `Create`/`Cancel` instruction construction, or
  the `i64::MAX` → Hydra-infinite iteration mapping;
- interval validation, millisecond-to-slot conversion, or start-slot
  selection;
- sponsor/signer/payer selection, blockhash source, RPC endpoint, or
  send-and-forget submit behavior;
- startup/shutdown wiring in `bins/magicblock-validator/src/leader.rs` or
  `nucleus` `Service::TaskScheduler` handling;
- task scheduler unit tests or `test-integration/test-task-scheduler`.

Because this crate consumes task requests emitted by Magic Program execution
and creates Hydra cranks, also update this file when `magicblock-program`,
`magicblock-magic-program-api`, engine service-message publishing,
`nucleus::tls::TlsManager`, or `hydra-api` ephemeral `Create`/`Cancel`
semantics change.

For the general documentation-update rule, see
`.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

| Path | Role |
| --- | --- |
| `magicblock-task-scheduler/Cargo.toml` | Package metadata. Depends on `engine`, `hydra-api` (`client`, `ephemeral`), `keeper`, `magicblock-program`, `nucleus`, Solana transaction/RPC crates, Tokio, and wincode. |
| `magicblock-task-scheduler/src/lib.rs` | Public crate surface. Re-exports `crank_pubkey`, `TaskSchedulerError`, and `TaskSchedulerService`. |
| `magicblock-task-scheduler/src/service.rs` | Runtime service: service-message loop, schedule/cancel processing, Hydra submit, and shutdown. |
| `magicblock-task-scheduler/src/crank.rs` | Crank PDA derivation, Hydra `Create` instruction builder, interval validation, and millisecond-to-slot conversion. |
| `magicblock-task-scheduler/src/errors.rs` | `TaskSchedulerError` and `TaskSchedulerResult`. Live variants are RPC and Keeper errors; several leftover variants are unused. |
| `bins/magicblock-validator/src/leader.rs` | Constructs the service with the engine, Aperture HTTP URL, and `engine.blockstore.blocktime`, then starts it under `Service::TaskScheduler`. |
| `programs/magicblock/src/schedule_task/` | Magic Program processors that validate schedule/cancel instructions and enqueue `TaskRequest`s through `TlsManager`. |
| `magicblock-magic-program-api/src/args.rs` | `TaskRequest`, `ScheduleTaskRequest`, and `CancelTaskRequest` wire types. |
| `test-integration/test-task-scheduler/` | Integration tests that start a leader with the ephemeral Hydra program preloaded. |

`magicblock-task-scheduler/README.md` and `docs/task-scheduler.md` still
describe the pre-Hydra SQLite delay-queue design. Do not treat them as
current until they are rewritten.

Main consumers:

- `mbv-leader` (`bins/magicblock-validator`), which owns construction and
  startup;
- Magic Program schedule/cancel instructions, which define the request
  semantics this crate turns into Hydra cranks;
- integration tests that locate cranks via `crank_pubkey`.

There is no `TaskSchedulerConfig`, `SchedulerDatabase`, `db.rs`, or
`ExecuteTask` path. `magicblock-config` no longer has scheduler reset,
min-interval, or failed-record retention settings.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exposes:

- `pub mod crank`, `pub mod errors`, and `pub mod service`;
- `pub use crank::crank_pubkey`;
- `pub use errors::TaskSchedulerError`;
- `pub use service::TaskSchedulerService`.

### `crank_pubkey`

`crank_pubkey(authority, task_id)` derives the Hydra crank PDA. The seed is
`sha256(authority || task_id.to_le_bytes())`; the PDA is
`hydra_api::instruction::ephemeral::find_crank_pda(&seed)`. Each authority
has its own namespace: the same `task_id` under a different authority is a
different crank.

### `TaskSchedulerService`

`TaskSchedulerService::new(engine, self_rpc_url, slot_interval)` subscribes
to `engine.transactions().subscribe_service_messages()`, stores the engine
handle (accounts, blockhash/slot, signer), and builds a nonblocking
`RpcClient` pointed at `self_rpc_url`.

`run(self, shutdown)` owns the service for its lifetime and reports a
terminal `ShutdownReason` through the `nucleus` handle:

- `Error` if the loop returns a service-level error;
- `Signalled` if the loop exits because shutdown was requested;
- `Unexpected` if the loop exits without a shutdown request (for example the
  service-message stream closed).

Internally the service owns:

- an `mpsc::Receiver<Vec<u8>>` of engine service messages;
- an `Engine` for account loads, current slot, latest blockhash, and the
  validator signer;
- an `RpcClient` for send-and-forget local submission;
- the validator slot interval used to convert millisecond cadences into
  Hydra slot intervals.

The type has manual `unsafe impl Send`/`Sync` with an explicit safety
comment: the service is moved into one Tokio task by `run()` and is not
cloned. Do not make it shared or mutated from multiple tasks without
revisiting this assumption.

### Errors

Live failures from the current loop are `TaskSchedulerError::RpcClient` (Hydra
submit) and `TaskSchedulerError::Keeper` (service-message subscribe). Schedule
and cancel processing errors are logged and treated as recoverable; they do
not stop the loop.

`InvalidConfiguration`, `UnauthorizedReplacing`, `SizeMismatch`,
`CrankWorker`, `Instruction`, `Wincode`, `TransactionExecution`, and `Io`
are leftover variants from the SQLite delay-queue implementation and are not
constructed by the current service.

## Runtime flows

### Startup

```text
Leader::try_from_config
  -> TaskSchedulerService::new(
       engine,
       config.aperture.listen.http(),
       config.engine.blockstore.blocktime,
     )
Leader start
  -> tokio::spawn(task_scheduler.run(shutdown.handle(Service::TaskScheduler)))
```

The leader always constructs and starts the service. There is no
primary/replica gate and no SQLite open, reset, or legacy-task migration in
this crate.

### Schedule request flow

```text
Magic Program ScheduleTask instruction
  -> TlsManager::enqueue(TaskRequest::Schedule)
  -> engine publishes the encoded request after the transaction commits
  -> TaskSchedulerService deserializes TaskRequest
  -> process_schedule_request
  -> send_create (optional cancel + Hydra Create)
  -> RpcClient::send_transaction
```

Processing details:

1. Invalid intervals (`<= 0` or `>= u32::MAX`) are ignored. The Magic
   Program also validates intervals; the service keeps this guard for
   channel inputs.
2. `iterations <= 0` is ignored.
3. `i64::MAX` iterations is the Magic API spelling of "run forever" and is
   sent to Hydra as `remaining = 0`. Passing the raw `i64::MAX` count would
   create a finite crank of ~9.2e18 executions.
4. `start_slot` is the engine's current slot. Cadence is
   `interval_slots(interval_millis, slot_interval)` (ceiling division, one
   slot minimum).
5. If a Hydra-owned account already exists at the PDA, the transaction is
   `[Cancel, Create]` so a reschedule replaces the crank atomically.
6. The validator identity is sponsor, fee payer, and Hydra cancel
   authority. User task authority is used only to derive the PDA.

Scheduled instruction signer flags are dropped when building `CreateArgs`.
Hydra rejects scheduled instructions that declare signers; the Magic Program
already rejects signer accounts and validator-authority accounts in the
payload.

### Cancel request flow

```text
Magic Program CancelTask instruction
  -> TlsManager::enqueue(TaskRequest::Cancel)
  -> engine publishes after commit
  -> process_cancel_request
  -> Hydra Cancel to crank_pubkey(authority, task_id)
  -> remaining lamports return to the validator identity
```

The service does not check whether the crank exists and does not compare
authorities against persisted state. Signer validation happens in the Magic
Program. A cancel for a missing crank fails at RPC submit and is logged.

### What this crate no longer does

Hydra, not this service, fires due cranks. There is no local `DelayQueue`,
no `ExecuteTask` transaction, no retry/backoff, no failed-record tables, and
no cleanup ticker. Completions, remaining iterations, and slot cadence live
in the Hydra crank account.

`legacy_start_slot` in `crank.rs` is an unused leftover from the SQLite-to-
Hydra migration and is not called by the service.

## Important internals and caveats

### Deterministic crank identity

Cancel and reschedule are lookups by PDA, not by a local task table. Changing
`crank_pubkey` seed layout silently orphans existing Hydra accounts.

### Validator-sponsored Hydra authority

`CreateArgs.authority` is the validator sponsor, not the user task
authority. That is why this service must issue Hydra `Cancel`: only the
sponsor can close the crank. Do not change signer layout, payer selection, or
cancel recipient without checking `hydra-api` ephemeral `create`/`cancel`
and `test_undrained_sponsor.rs`.

Submit is send-and-forget. The service relies on the identity account write
lock to serialize overlapping sponsor transactions rather than waiting for
confirmation.

### Service-message filtering

The engine stream carries every service message. Deserialization failure is
treated as "not a `TaskRequest`" and ignored. Do not log or fail the loop on
unrecognized payloads.

### Leftover helpers and error variants

`legacy_start_slot` and several `TaskSchedulerError` variants are unused by
the live path. Do not revive SQLite persistence, optimistic `updated_at`
tokens, or unauthorized-replacement DB checks unless the Hydra model itself
changes.

## Important invariants

1. The service must remain stateless: crank identity is `crank_pubkey
   (authority, task_id)`.
2. A `TaskRequest` must be applied only after the originating transaction
   commits (engine service-message publish).
3. Non-`TaskRequest` service messages must be ignored.
4. Invalid intervals and `iterations <= 0` must be no-ops.
5. `iterations == i64::MAX` must map to Hydra infinite (`remaining = 0`).
6. Reschedule of an existing Hydra-owned crank must cancel then create in
   one transaction.
7. Cancel must refund remaining crank lamports to the validator sponsor.
8. Hydra `Create` authority must be the validator sponsor so only this
   service can cancel through Hydra.
9. Scheduled instruction signer flags must not be forwarded to Hydra.
10. Create/cancel transactions must use the engine signer, the latest engine
    blockhash, and the local Aperture HTTP endpoint.
11. Different authorities with the same `task_id` must get independent
    cranks.
12. Shutdown must report a terminal `ShutdownReason` through the nucleus
    handle rather than exiting silently.
13. Changes must avoid unnecessary RPC amplification, long-held identity
    locks, and unbounded logging on the schedule/cancel path.

## Common change areas and what to inspect

### Changing schedule/cancel semantics

Start with `magicblock-task-scheduler/src/service.rs`
(`process_request`, `process_schedule_request`, `process_cancel_request`)
and `src/crank.rs` (`crank_pubkey`, `build_create_ix`,
`is_valid_task_interval`, `interval_slots`). Then inspect
`programs/magicblock/src/schedule_task/process_schedule_task.rs`,
`process_cancel_task.rs`, `mod.rs` (`validate_cranks_instructions`), and
`magicblock-magic-program-api/src/args.rs`.

Validate interval/iteration guards, per-authority namespacing, reschedule
cancel+create, missing-crank cancel, and signer-flag stripping. Integration
tests: `test_schedule_task.rs` and `test_undrained_sponsor.rs`.

### Changing Hydra instruction construction or submit behavior

Start with `send_create`, `send_cancel`, `submit`, and `build_create_ix`.
Inspect `hydra-api` ephemeral `create`/`cancel`, `CreateArgs` (`remaining`,
`authority`, scheduled metas), and leader wiring for RPC URL and blocktime.

Check sponsor, cancel recipient, infinite-iteration mapping, blockhash
source, and send-and-forget races on the identity account.

### Changing startup/shutdown

Inspect `TaskSchedulerService::new`, `run`, `run_loop`, and
`bins/magicblock-validator/src/leader.rs`. Preserve nucleus
`Service::TaskScheduler` termination: a service-level error must surface as
`ShutdownReason::Error` rather than leaving the leader running without
scheduling.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust
  checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md`
  or `mbv-check`; include focused package checks for
  `magicblock-task-scheduler`.
- Schedule/cancel behavior that creates or funds Hydra accounts should also
  run `test-integration/test-task-scheduler` (`test_schedule_task`,
  `test_undrained_sponsor`). Those tests preload the ephemeral Hydra program
  from `test-integration/programs/hydra/hydra.so`.
- Performance-sensitive changes should report whether service-message drain
  latency, RPC send volume, or identity-account lock hold time was measured
  or only reasoned about.

## Adjacent implementation references

- See `.agents/context/crates/mbv-leader.md` for the leader lifecycle that
  constructs and starts this service.
- Refer to `.agents/context/crates/magicblock-magic-program-api.md` for
  `TaskRequest` and Magic Program instruction wire types. That guide still
  mentions `ExecutionTlsStash`; the live enqueue path is
  `nucleus::tls::TlsManager`.
- Hydra crate docs and `hydra-api` ephemeral instruction types own crank
  execution, remaining-iteration accounting, and on-chain cancel
  constraints.
