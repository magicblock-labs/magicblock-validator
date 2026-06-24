# High-Level Architecture

This file explains the repository-level architecture and how major crate groups interact. It intentionally stays high level. Detailed/lower-level architecture belongs in crate-specific docs under `.agents/context/crates/` as those files are added. For crate-by-crate ownership, use `.agents/context/crate-map.md`.

## System shape

The validator is a service graph around one core loop: make the right accounts available locally, execute valid ER transactions, persist the result, and settle scheduled state changes back to Solana. This graph is performance-sensitive: architectural changes must preserve low-latency, high-throughput behavior on critical paths unless there is no viable alternative, and any unavoidable tradeoff must be called out explicitly. See `.agents/rules/validator-goals.md` and `.agents/specs/validator-specification.md` for binding security details.

```text
Client / Operator
      |
      v
RPC / TUI / Admin ingress
      |
      v
Validator orchestration
      |
      +--> account synchronization <----> Solana RPC/WS + delegation metadata
      |
      +--> transaction execution -----> local AccountsDb + Ledger -----> events
      |
      +--> commit/undelegation -------> base-layer transactions
      |
      +--> task scheduling -----------> submitted transactions
      |
      +--> replication/metrics -------> replicas + observability
```

## Main layers

### 1. Process and service orchestration

Owned primarily by `magicblock-validator`, `magicblock-api`, and `magicblock-config`.

Responsibilities:

- parse/load configuration,
- construct the validator service graph,
- open persistent stores,
- initialize account sync, RPC, scheduler, committor, task scheduler, replication, metrics, and admin support,
- recover persisted work,
- coordinate startup mode versus primary/replica mode,
- stop services and flush state in the correct order.

Architecture rule: process entrypoints should stay thin; cross-service wiring belongs in the orchestration layer, not in leaf crates.

### 2. Client/API ingress

Owned primarily by `magicblock-aperture` plus admin/TUI support crates.

Responsibilities:

- expose Solana-compatible JSON-RPC and websocket/pubsub behavior,
- accept transactions and simulations,
- serve account/ledger/status reads,
- trigger just-in-time account availability work for local misses,
- forward validator events to clients/subscribers.

Architecture rule: the RPC layer should route work to account sync and execution services; it should not duplicate execution, delegation, or commit protocol logic. Keep per-request work lean and avoid blocking critical request paths.

### 3. Account synchronization

Owned primarily by `magicblock-chainlink`, `magicblock-account-cloner`, and `magicblock-accounts`.

Responsibilities:

- determine whether required accounts are delegated, undelegated/read-only, fee-payers, programs, or missing/stale,
- fetch base-layer account data and delegation metadata,
- subscribe to remote changes where needed,
- materialize local account/program state,
- provide account availability to RPC and transaction execution,
- hand scheduled commit work toward settlement.

Architecture rule: this layer prepares local state for execution. It should not decide post-execution account access rules; those belong to the execution/SVM path. Avoid fetch amplification, duplicate clone work, subscription churn, and unnecessary serialization in account availability paths.

### 4. Transaction execution

Owned primarily by `magicblock-processor`, `magicblock-core`, the local storage crates, and the forked SVM dependency.

Responsibilities:

- receive processable transactions,
- acquire account locks,
- schedule work onto executors,
- run SVM execution,
- enforce MagicBlock access validation,
- commit local account changes,
- write ledger/status records,
- emit account, transaction, slot, and replication events.

Architecture rule: execution must preserve the writable-account invariant and avoid mixing scheduler/account-lock concerns with RPC or commit-delivery concerns. It must also preserve scheduler/executor parallelism and avoid avoidable latency, contention, allocation, or I/O regressions in the hot path.

### 5. Local persistence

Owned primarily by `magicblock-accounts-db` and `magicblock-ledger`.

Responsibilities:

- store local account state,
- index accounts for execution/RPC,
- support snapshots/maintenance,
- store transaction, status, block, address-signature, and blockhash history,
- support recovery and user-visible RPC history.

Architecture rule: maintenance operations that can race execution must be coordinated with scheduler pausing.

### 6. Base-layer settlement

Owned primarily by `magicblock-program`, `magicblock-magic-program-api`, `magicblock-committor-service`, `magicblock-committor-program`, `magicblock-table-mania`, and `magicblock-rpc-client`.

Responsibilities:

- let programs schedule commits, commit-and-undelegate operations, intent bundles, and Magic Actions,
- persist and recover pending settlement work,
- build valid base-layer transactions,
- handle address lookup tables and large changesets,
- send/confirm base-layer transactions,
- keep local lifecycle state consistent with scheduled undelegation.

Architecture rule: Magic Program instructions schedule intent; validator services realize that intent on the base layer.

### 7. Background services

Owned by task scheduler, replicator, metrics, admin, and shared service crates.

Responsibilities:

- execute scheduled program tasks,
- replicate primary output to replicas,
- expose metrics/admin/operator hooks,
- provide reusable service infrastructure.

Architecture rule: background services should integrate through shared channels/service APIs rather than reaching through unrelated crate internals.

## Important interaction patterns

### Transaction submission path

```text
RPC/router ingress
  -> account synchronization ensures required accounts exist locally
  -> processor scheduler locks accounts
  -> executor runs SVM
  -> AccountsDb and Ledger persist results
  -> events notify RPC subscriptions, metrics, replication, and other consumers
```

### First use of delegated state

```text
base-layer delegation exists
  -> ER read/transaction needs account
  -> account sync fetches account + delegation metadata
  -> cloner installs local representation
  -> processor can execute valid transactions against it
```

### Commit / undelegation path

```text
program invokes Magic Program in ER
  -> MagicContext records scheduled intent
  -> validator-side processing picks up intent
  -> committor builds/sends base-layer transaction(s)
  -> commit keeps delegation active OR undelegation returns ownership after settlement
```

### Startup path

```text
load config
  -> open ledger/accounts storage
  -> initialize services
  -> recover persisted work
  -> replay/repair local state where configured
  -> enter primary or replica execution mode
```

### Shutdown path

```text
cancel services
  -> protect/finish in-flight work where required
  -> join threads/runtimes
  -> flush persistent stores
```

## Boundaries .agents should preserve

- **RPC ingress is not the protocol source of truth.** It should call into account sync, execution, and storage layers.
- **Account synchronization is not transaction execution.** It prepares accounts; execution validates and commits changes.
- **Magic Program scheduling is not base-layer settlement.** It records intent; committor services deliver it.
- **Local persistence is shared infrastructure.** Coordinate maintenance with execution.
- **Replication observes/replays validator output.** Do not make primary and replica modes accidentally diverge.
- **Crate-specific details belong in crate docs.** Keep this file focused on cross-crate architecture.
- **Performance is part of the architecture contract.** Do not move heavy work into RPC, account sync, scheduler/executor, persistence, or settlement hot paths without an explicit justification and mitigation plan.
- **Security boundaries must remain explicit between layers.** RPC ingress handles untrusted input and must not become a path that bypasses execution/SVM validation or account-sync correctness.
