# Crate Map

This map helps .agents find the right crate before making changes. The dependency lists focus on workspace crates and are intentionally concise; external Solana/SVM dependencies are omitted.

The validator is performance-sensitive. When changing any crate on RPC, account synchronization, scheduling/execution, persistence, replication, or settlement paths, preserve low-latency and high-throughput behavior. Avoid unnecessary blocking, allocation, lock contention, I/O, serialization, logging, and duplicate work; explicitly call out any unavoidable performance tradeoff.

## Core validator crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-validator` | Main validator binary and process entrypoint. | `magicblock-api`, `magicblock-config`, `magicblock-core`, `magicblock-tui-client`, `magicblock-version` | End users/operators | Parses config, builds runtime, starts headless/TUI validator; see `.agents/context/crates/magicblock-validator.md` before changing this crate. |
| `magicblock-api` | Top-level service orchestration and `MagicValidator` implementation. | engine/keeper, aperture, chainlink, committor, config, deprecated ledger, task scheduler, admin/services | `magicblock-validator` | Owns the engine handle and validator-side startup/shutdown wiring; see `.agents/context/crates/magicblock-api.md` before changing this crate. |
| `magicblock-config` | Validator configuration model and layered config loading. | none | Most service crates | CLI/env/TOML/default config source; see `.agents/context/crates/magicblock-config.md` before changing configurable behavior. |
| `magicblock-core` | Shared channels, traits, account locks/helpers, intent/core types. | `magicblock-magic-program-api` | Most runtime crates | Central wiring layer; changes can affect scheduler, RPC, ledger, services, replication. See `.agents/context/crates/magicblock-core.md` before changing this crate. |
| `magicblock-version` | Build/version metadata. | none | `magicblock-validator`, `magicblock-aperture` | Keep version reporting stable for RPC/operator tooling; see `.agents/context/crates/magicblock-version.md` before changing this crate. |

## RPC, API, and operator-facing crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-aperture` | Solana-compatible JSON-RPC and websocket/pubsub server. | engine, chainlink, config, deprecated ledger, metrics, version | `magicblock-api` | Uses engine for live state/submission and deprecated ledger only for historical fallback. See `.agents/context/crates/magicblock-aperture.md` before changing this crate. |
| `magicblock-rpc-client` | RPC client utilities for sending/confirming base-layer transactions. | `magicblock-metrics` | committor, table-mania, account-cloner, API/admin | Critical for base-layer commit delivery; see `.agents/context/crates/magicblock-rpc-client.md` before changing this crate. |
| `magicblock-validator-admin` | Admin/client helpers for validator management operations. | `magicblock-program`, `magicblock-rpc-client` | `magicblock-api` | Keep compatible with operator/admin workflows; see `.agents/context/crates/magicblock-validator-admin.md` before changing this crate. |
| `magicblock-tui-client` | TUI client/binary support. | none | `magicblock-validator` | UI-facing; should not own core validator logic. |

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
| `magicblock-chainlink` | Base-chain account/delegation coordination. | accounts-db, AML, config, core, magic-program API, metrics | account-cloner, aperture, API, magic program, services | Checks/clones remote accounts, tracks delegation state, coordinates base-layer reads and observed undelegation requests. See `.agents/context/crates/magicblock-chainlink.md` before changing this crate. |
| `magicblock-aml` | External/cached risk-scoring integration. | `magicblock-config` (dev: `magicblock-core`) | `magicblock-chainlink` | Optional Range risk checks for post-delegation action signers; see `.agents/context/crates/magicblock-aml.md` before changing this crate. |

## Commit and base-layer settlement crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-committor-service` | Executes scheduled base-layer intents: commit, undelegate, finalize, action. | committor program, core, metrics, magic program, rpc-client, table-mania | account-cloner, API | Durable commit pipeline; accepts scheduled intents, handles recovery, transaction prep, buffers, ALTs, confirmations. See `.agents/context/crates/magicblock-committor-service.md` before changing this crate. |
| `magicblock-committor-program` | On-chain committor program. | none | `magicblock-committor-service` | Base-layer program side for changeset buffers/commit application; see `.agents/context/crates/magicblock-committor-program.md` before changing this crate. |
| `magicblock-table-mania` | Address lookup table management. | metrics, rpc-client | `magicblock-committor-service` | Creates/extends/deactivates/closes ALTs needed by commit transactions. See `.agents/context/crates/magicblock-table-mania.md` before changing this crate. |

## Magic Program and shared protocol crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-program` | Magic Program implementation (`programs/magicblock`). | chainlink, core, magic-program API | account-cloner, accounts, API, committor, processor, task scheduler, admin | Implements scheduling, cloning, ephemeral accounts, validator-only operations. |
| `magicblock-magic-program-api` | Shared Magic Program instruction, PDA, args, and compatibility types. | none | core, accounts-db, chainlink, processor, magic program, services, cloner, API | Use this instead of duplicating Magic Program wire types; see `.agents/context/crates/magicblock-magic-program-api.md` before changing this crate. |

## Scheduling, replication, services, and observability

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-task-scheduler` | Program-scheduled task/crank service. | engine, config, core, magic program | `magicblock-api` | SQLite-backed delay queue, retries/backoff, and engine transaction submission. See `.agents/context/crates/magicblock-task-scheduler.md` before changing this crate. |
| `magicblock-services` | Shared validator services/adapters. | engine, chainlink, core, magic-program API, metrics, magic program | `magicblock-api` | Callback adapter and owner-program undelegation request observer. See `.agents/context/crates/magicblock-services.md` before changing this crate. |
| `magicblock-metrics` | Metrics helpers and instrumentation. | none | RPC, ledger, processor, chainlink, committor, table-mania, API | Prefer adding observability here rather than ad-hoc metrics code. See `.agents/context/crates/magicblock-metrics.md` before changing this crate. |

## Tools and test support

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `genx` | Developer/tooling binary. | `magicblock-accounts-db` | manual/tooling use | Keep outside runtime-critical paths. |
| `ledger-stats` | Ledger/accounts statistics tool. | accounts-db, core, ledger | manual/tooling use | Useful for inspecting local persisted state. |
| `keypair-base58` | Keypair conversion/helper binary. | none | manual/tooling use | Small standalone operator/dev helper. |

## How to use this map

- For transaction correctness, start with `magicblock-processor`, then inspect `magicblock-accounts-db`, `magicblock-ledger`, and `magicblock-program` interactions.
- For delegation or account cloning bugs, start with `magicblock-chainlink` and `magicblock-account-cloner`; include `magicblock-services` when observed undelegation requests are involved.
- For commit or undelegation bugs, start with `magicblock-program`, `magicblock-committor-service`, and `magicblock-services` for request-triggered scheduling.
- For RPC behavior, start with `magicblock-aperture`; check `magicblock-chainlink` if reads trigger cloning.
- For validator lifecycle/startup/shutdown, start with `magicblock-api` and `magicblock-validator`.
- When adding, removing, renaming, or repurposing a crate, update this file and `AGENTS.md` in the same change.
- When changing crate responsibilities, note whether performance-sensitive work moved onto or off of a hot path and document any expected regression or mitigation.
