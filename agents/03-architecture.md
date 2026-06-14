# High-Level Architecture

This document describes the repository-level architecture of the MagicBlock validator and how the major crates interact. It intentionally stays high level. Lower-level architecture, module walkthroughs, important types, and crate-specific invariants should live in the dedicated crate docs under `agents/crates/` as those files are added.

## Architectural purpose

The validator is a specialized SVM runtime for Ephemeral Rollups. It coordinates local transaction execution for delegated and ephemeral state, local persistence, RPC access, base-layer account synchronization, commit/undelegation settlement, scheduled tasks, and optional primary/replica replication.

The system is organized around a few large responsibilities:

1. **Process orchestration** — start, stop, configure, and wire services.
2. **Client/API ingress** — accept RPC, websocket, and transaction requests.
3. **Account availability** — ensure base-layer accounts/programs are cloned locally when needed.
4. **Transaction execution** — schedule, lock, execute, validate, persist, and emit results.
5. **Base-layer settlement** — commit ER state, undelegate accounts, and run post-commit actions.
6. **Persistence and recovery** — maintain local account state, ledger history, task state, and pending commit state.
7. **Replication and observability** — stream state/events to replicas and expose metrics/status.

## Layered view

```text
Operator / Client
      |
      v
magicblock-validator
      |
      v
magicblock-api  -----------------------------+
      |                                      |
      | wires services                       | lifecycle / shutdown / recovery
      v                                      |
+----------------------+---------------------+----------------------+
| RPC/API              | Execution           | Base-chain sync      |
| magicblock-aperture  | magicblock-processor| magicblock-chainlink |
|                      | magicblock-core     | magicblock-account-  |
|                      |                     | cloner               |
+----------------------+---------------------+----------------------+
      |                         |                    |
      v                         v                    v
Subscriptions/events      AccountsDb + Ledger    Remote Solana RPC/WS
                           |                    Delegation records
                           v
                    magicblock-accounts-db
                    magicblock-ledger
                           |
                           v
                    Commit / tasks / replication
                    magicblock-accounts
                    magicblock-committor-service
                    magicblock-task-scheduler
                    magicblock-replicator
```

## Entry and orchestration

### `magicblock-validator`

`magicblock-validator` is the main binary. It should remain a thin process entrypoint:

- load CLI/config inputs,
- initialize the Tokio runtime,
- construct the validator through `magicblock-api`,
- run headless or with the TUI,
- handle process signals and shutdown.

It should not accumulate core protocol logic.

### `magicblock-api`

`magicblock-api` is the top-level orchestrator. It owns the `MagicValidator` service graph and wires together:

- configuration,
- ledger,
- AccountsDb,
- RPC server,
- transaction scheduler,
- account cloning/chainlink,
- committor service,
- task scheduler,
- replication,
- metrics,
- admin utilities,
- startup recovery and shutdown ordering.

When a change affects cross-service lifecycle behavior, startup/shutdown ordering, or how services are connected, start here.

### `magicblock-config`

`magicblock-config` defines the validator configuration surface. New runtime behavior that needs operator control should generally add configuration here rather than using ad-hoc environment reads elsewhere.

## Client ingress and RPC layer

### `magicblock-aperture`

`magicblock-aperture` provides the validator's Solana-compatible JSON-RPC and websocket/pubsub surface. It handles:

- standard Solana RPC-style requests,
- MagicBlock-specific request behavior,
- transaction submission,
- account/program/signature/log/slot subscriptions,
- forwarding validator events to subscribers,
- local read misses that may trigger account cloning.

A typical client transaction enters through aperture, which asks the account synchronization layer to ensure required accounts are present, then forwards the transaction into the processor scheduler.

## Account synchronization layer

### `magicblock-chainlink`

`magicblock-chainlink` coordinates base-layer knowledge. It decides whether accounts are delegated, undelegated, missing, or stale and coordinates remote fetch/subscription behavior.

Its job is not to execute transactions. Its job is to ensure the execution layer sees the right local account representation before execution.

### `magicblock-account-cloner`

`magicblock-account-cloner` materializes remote accounts and programs into local validator state. It distinguishes key account flavors:

- fee-payer accounts,
- read-only undelegated accounts,
- writable delegated accounts,
- program accounts,
- large accounts requiring chunked clone paths.

The cloner interacts with the Magic Program and local scheduler to inject cloned state into the validator.

### `magicblock-accounts`

`magicblock-accounts` sits at the boundary between account availability and commit processing. It ensures accounts required for execution are available and feeds scheduled commit work into the committor pipeline.

## Execution layer

### `magicblock-core`

`magicblock-core` is the shared wiring layer. It contains common channel endpoints, traits, event types, and shared structures used across RPC, scheduler, ledger, services, and replication.

Changes here tend to ripple through many crates, so keep abstractions stable and narrowly scoped.

### `magicblock-processor`

`magicblock-processor` is the local transaction execution engine. It owns:

- transaction scheduling,
- account locking,
- executor worker coordination,
- SVM execution,
- local account commit,
- ledger/status writes,
- transaction/account events,
- slot/blockhash transitions from the execution perspective.

It depends on the forked SVM behavior and must preserve the MagicBlock access invariant: ordinary ER execution must only write accounts that are delegated, ephemeral, confined, or explicitly allowed by protocol rules.

### Forked SVM dependencies

The validator relies on a forked SVM with MagicBlock-specific account representation and access validation. Most code in this repository should treat the fork as the execution primitive and avoid duplicating access-control rules outside the intended boundaries.

## Storage layer

### `magicblock-accounts-db`

`magicblock-accounts-db` stores local account state. It is a custom account database, not Agave's accounts-db. It supports account indexing, snapshots, defragmentation, and checksum-style maintenance.

Execution, cloning, replication, tests, and RPC reads all depend on this crate. Maintenance operations must be coordinated with scheduler pausing so account state is not mutated concurrently.

### `magicblock-ledger`

`magicblock-ledger` stores local transaction, status, block, blockhash, performance, and address-signature history. It also provides latest block information used by RPC and execution.

The ledger is part of validator recovery and user-visible RPC behavior, so changes to recording or truncation affect both correctness and observability.

## Base-layer settlement layer

### `magicblock-program`

`magicblock-program` is the Magic Program implementation used inside the ER. It provides protocol instructions for:

- commit scheduling,
- commit-and-undelegate scheduling,
- intent bundles,
- scheduled tasks,
- ephemeral account create/resize/close,
- clone support,
- validator-only operations.

Program instructions stage intent; they do not by themselves perform all base-layer settlement.

### `magicblock-magic-program-api`

`magicblock-magic-program-api` contains shared wire types, instruction args, PDA helpers, and compatibility types for the Magic Program. Other crates should use this crate instead of re-declaring Magic Program instruction formats.

### `magicblock-committor-service`

`magicblock-committor-service` realizes scheduled base-layer intents. It turns commit, undelegation, finalize, and action intents into valid Solana transactions, handles confirmation, and persists in-flight state for recovery.

It coordinates with:

- `magicblock-rpc-client` for sending and confirming base-layer transactions,
- `magicblock-table-mania` for address lookup tables,
- `magicblock-committor-program` for base-layer commit program behavior,
- local commit intent sources through `magicblock-accounts` / `magicblock-api`.

### `magicblock-committor-program`

`magicblock-committor-program` is the on-chain base-layer program side used by the committor flow, especially for buffered changesets and commit application.

### `magicblock-table-mania`

`magicblock-table-mania` manages address lookup tables required by larger or more complex base-layer transactions.

## Scheduled tasks

### `magicblock-task-scheduler`

`magicblock-task-scheduler` executes program-scheduled cranks/tasks. It persists scheduled tasks, retries failures with backoff, and submits transactions when tasks become due.

This is separate from the transaction scheduler in `magicblock-processor`. The processor scheduler decides when local transactions run; the task scheduler decides when program-defined future work should be submitted.

## Replication and high availability

### `magicblock-replicator`

`magicblock-replicator` streams validator events/state between a primary and replicas over NATS JetStream. The primary publishes; replicas consume and replay.

Primary and replica execution modes intentionally differ. Primary execution can exploit parallel scheduling; replica replay must preserve the ordering needed to match primary output and detect divergence.

## Metrics, services, admin, and support crates

- `magicblock-metrics` provides shared instrumentation.
- `magicblock-services` contains reusable service helpers/adapters.
- `magicblock-validator-admin` supports admin/operator interactions.
- `magicblock-rpc-client` is shared by components that talk to base-layer RPC.
- `test-kit`, `guinea`, and tools crates support tests and development workflows.

## Common end-to-end interactions

### Transaction submission

1. Client submits a transaction through RPC/router-facing infrastructure.
2. RPC layer checks local state and asks account synchronization to clone/fetch as needed.
3. Transaction is sent to the processor scheduler.
4. Scheduler locks accounts and dispatches to an executor.
5. Executor runs SVM, validates access, commits account changes locally, records ledger/status, and emits events.
6. RPC subscriptions and other consumers receive emitted events.

### Delegated account first use

1. Account is delegated on the base layer.
2. No local ER account may exist yet.
3. First ER read or transaction triggers chainlink/cloner.
4. Cloner fetches account data and delegation metadata.
5. Account is installed locally with the representation expected by ER execution.
6. Processor can execute valid transactions against it.

### Commit / commit-and-undelegate

1. User program invokes Magic Program commit or intent-bundle instruction inside ER execution.
2. Magic Program records scheduled intent information in MagicContext.
3. Validator-side commit processing picks up scheduled intents.
4. Committor service builds and sends base-layer transactions.
5. For commit, the account remains delegated.
6. For commit-and-undelegate, local state transitions to undelegating/immutable and base-layer ownership is eventually restored to the original program.

### Startup and recovery

1. Validator opens persistent stores.
2. Services are constructed and connected.
3. Programs and local state are initialized/recovered.
4. Pending commit intents and persisted service state are recovered.
5. Scheduler transitions from startup mode into primary or replica mode.

### Shutdown

1. Cancellation begins.
2. Services stop in an order that protects in-flight work.
3. Threads and runtimes join.
4. Persistent stores flush.

## Guidance for future crate docs

This file should remain a high-level map. Do not add detailed module-by-module descriptions here. Put lower-level architecture into crate-specific files under `agents/crates/`, including:

- important modules,
- main public types,
- runtime interactions,
- invariants,
- common change areas,
- crate-local tests and validation commands.
