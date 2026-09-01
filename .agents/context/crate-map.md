# Crate Map

This map helps .agents find the right crate before making changes. The dependency lists focus on workspace crates and are intentionally concise; external Solana/SVM dependencies are omitted.

The validator is performance-sensitive. When changing any crate on RPC, account synchronization, scheduling/execution, persistence, replication, or settlement paths, preserve low-latency and high-throughput behavior. Avoid unnecessary blocking, allocation, lock contention, I/O, serialization, logging, and duplicate work; explicitly call out any unavoidable performance tradeoff.

## Core validator crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `mbv-leader` | Leader process and full validator service orchestration. | engine/keeper, aperture, chainlink, committor, config, deprecated ledger, task scheduler, admin/services, `magicblock-runtime` | End users/operators | Owns `Engine<Leader>` and the leader lifecycle; see `.agents/context/crates/mbv-leader.md`. |
| `mbv-verifier` | Bare follower process. | engine/keeper, replicator, config, metrics, `magicblock-runtime` | End users/operators | Owns `Engine<Follower>`, replication, process-lifetime metrics exposure, and snapshot-driven reopen; see `.agents/context/crates/mbv-verifier.md`. |
| `magicblock-runtime` | Shared Keeper runtime image. | engine/keeper, Magic Program, configured BPF programs | leader and verifier | Single owner of native builtins, loadable programs, and genesis accounts; see `.agents/context/crates/magicblock-runtime.md`. |
| `magicblock-config` | Validator configuration model and layered config loading. | none | Most service crates | CLI/env/TOML/default config source; see `.agents/context/crates/magicblock-config.md` before changing configurable behavior. |
| `magicblock-core` | Shared channels, traits, account locks/helpers, intent/core types. | `magicblock-magic-program-api` | Most runtime crates | Central wiring layer; changes can affect scheduler, RPC, ledger, services, replication. See `.agents/context/crates/magicblock-core.md` before changing this crate. |
| `magicblock-version` | Build/version metadata. | none | `mbv-leader`, `magicblock-aperture` | Keep version reporting stable for RPC/operator tooling; see `.agents/context/crates/magicblock-version.md` before changing this crate. |

## RPC, API, and operator-facing crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-aperture` | Solana-compatible JSON-RPC and websocket/pubsub server. | engine, chainlink, config, deprecated ledger, metrics, version | `mbv-leader` | Uses engine for live state/submission and deprecated ledger only for historical fallback. See `.agents/context/crates/magicblock-aperture.md` before changing this crate. |
| `magicblock-rpc-client` | RPC client utilities for sending/confirming base-layer transactions. | `magicblock-metrics` | committor, table-mania, API/admin | Critical for base-layer commit delivery; see `.agents/context/crates/magicblock-rpc-client.md` before changing this crate. |
| `magicblock-validator-admin` | Admin/client helpers for validator management operations. | `magicblock-program`, `magicblock-rpc-client` | `mbv-leader` | Keep compatible with operator/admin workflows; see `.agents/context/crates/magicblock-validator-admin.md` before changing this crate. |
| `mbv` | Manual operator CLI, including Magic Domain Program interactions. | config, MDP, Solana RPC | End users/operators | Does not participate in leader lifecycle; see `.agents/context/crates/mbv.md`. |
| `mbv-tui` | External RPC/websocket TUI. | Solana RPC/pubsub client libraries | End users/operators | UI-facing and independent of the leader process; see `.agents/context/crates/mbv-tui.md`. |

## Execution and storage crates

The primary execution, accountsdb, current ledger, keeper, and TCP replication
crates now live in the sibling `../engine` workspace. Read `../engine/AGENTS.md`
and the owning engine crate README before changing them. MBV depends on those
crates by path and keeps only the deprecated RocksDB ledger during the historical
RPC fallback period.

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-ledger` | Deprecated RocksDB history retained during migration. | `magicblock-core`, `magicblock-metrics`, `solana-storage-proto` | aperture, API | Read-only historical fallback after engine misses; it no longer owns execution state. |
| `solana-storage-proto` | Generated/protobuf storage support. | none | `magicblock-ledger` | Low-level ledger serialization support; see `.agents/context/crates/storage-proto.md` before changing this crate. |

## Delegation, cloning, and account lifecycle crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-chainlink` | Base-chain account/delegation coordination. | engine/keeper, AML, config, core, magic-program API, metrics | aperture, API, committor, magic program, services | Resolves and materializes remote accounts through engine accessors, uses engine-owned load/cache coordination, tracks subscriptions and delegation state, and coordinates observed undelegation requests. See `.agents/context/crates/magicblock-chainlink.md` before changing this crate. |
| `magicblock-aml` | External/cached risk-scoring integration. | `magicblock-config` (dev: `magicblock-core`) | `magicblock-chainlink` | Optional Range risk checks for post-delegation action signers; see `.agents/context/crates/magicblock-aml.md` before changing this crate. |

## Commit and base-layer settlement crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-committor-service` | Executes scheduled base-layer intents: commit, undelegate, finalize, action. | committor program, core, metrics, magic program, rpc-client, table-mania | API | Durable commit pipeline; accepts scheduled intents, handles recovery, transaction prep, buffers, ALTs, confirmations. See `.agents/context/crates/magicblock-committor-service.md` before changing this crate. |
| `magicblock-committor-program` | On-chain committor program. | none | `magicblock-committor-service` | Base-layer program side for changeset buffers/commit application; see `.agents/context/crates/magicblock-committor-program.md` before changing this crate. |
| `magicblock-table-mania` | Address lookup table management. | metrics, rpc-client | `magicblock-committor-service` | Creates/extends/deactivates/closes ALTs needed by commit transactions. See `.agents/context/crates/magicblock-table-mania.md` before changing this crate. |

## Magic Program and shared protocol crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-program` | Magic Program implementation (`programs/magicblock`). | chainlink, core, magic-program API | API, committor, services, task scheduler | Implements scheduling, ephemeral accounts, callbacks, and validator-only operations; legacy account-composition variants fail closed. |
| `magicblock-magic-program-api` | Shared Magic Program instruction, PDA, args, and compatibility types. | none | core, chainlink, magic program, services, API | Use this instead of duplicating Magic Program wire types; see `.agents/context/crates/magicblock-magic-program-api.md` before changing this crate. |

## Scheduling, replication, services, and observability

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-task-scheduler` | Program-scheduled task/crank service. | engine, hydra-api, keeper, magic program, nucleus | `mbv-leader` | Stateless Hydra crank create/cancel from committed Magic Program `TaskRequest`s. See `.agents/context/crates/magicblock-task-scheduler.md` before changing this crate. |
| `magicblock-services` | Shared validator services/adapters. | engine, chainlink, core, magic-program API, metrics, magic program | `mbv-leader` | Callback adapter and owner-program undelegation request observer. See `.agents/context/crates/magicblock-services.md` before changing this crate. |
| `magicblock-metrics` | Metrics helpers and combined Prometheus endpoint. | none | leader, verifier, RPC, ledger, chainlink, committor, table-mania | Its endpoint combines the namespaced MBV registry with Engine collectors from the process-wide default registry. See `.agents/context/crates/magicblock-metrics.md` before changing this crate. |

## Tools and test support

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `genx` | Developer/tooling binary. | `magicblock-accounts-db` | manual/tooling use | Keep outside runtime-critical paths. |
| `ledger-stats` | Ledger/accounts statistics tool. | accounts-db, core, ledger | manual/tooling use | Useful for inspecting local persisted state. |
| `keypair-base58` | Keypair conversion/helper binary. | none | manual/tooling use | Small standalone operator/dev helper. |

## How to use this map

- For transaction correctness, start with the sibling engine's processor, keeper, accountsdb, and ledger, then inspect `magicblock-program` interactions.
- For delegation or account materialization bugs, start with `magicblock-chainlink` and the engine account accessor; include `magicblock-services` when observed undelegation requests are involved.
- For commit or undelegation bugs, start with `magicblock-program`, `magicblock-committor-service`, and `magicblock-services` for request-triggered scheduling.
- For RPC behavior, start with `magicblock-aperture`; check `magicblock-chainlink` if reads trigger cloning.
- For leader lifecycle/startup/shutdown, start with `bins/mbv-leader`.
- For follower replication/reopen behavior, start with `bins/mbv-verifier`.
- For the shared execution image, start with `magicblock-runtime`.
- When adding, removing, renaming, or repurposing a crate, queue updates to this file and `AGENTS.md` for the weekly documentation-maintenance task.
- When changing crate responsibilities, note whether performance-sensitive work moved onto or off of a hot path and document any expected regression or mitigation.
