# Crate Map

This map helps agents find the right crate before making changes. The dependency lists focus on workspace crates and are intentionally concise; external Solana/SVM dependencies are omitted.

## Core validator crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-validator` | Main validator binary and process entrypoint. | `magicblock-api`, `magicblock-config`, `magicblock-core`, `magicblock-tui-client`, `magicblock-version` | End users/operators | Parses config, builds runtime, starts headless/TUI validator. |
| `magicblock-api` | Top-level service orchestration and `MagicValidator` implementation. | accounts, aperture, chainlink, committor, config, core, ledger, processor, replicator, task scheduler, admin/services | `magicblock-validator` | Owns startup/shutdown wiring for most services. |
| `magicblock-config` | Validator configuration model and layered config loading. | none | Most service crates | CLI/env/TOML/default config source; update when adding configurable behavior. |
| `magicblock-core` | Shared channels, traits, account locks/helpers, intent/core types. | `magicblock-magic-program-api` | Most runtime crates | Central wiring layer; changes can affect scheduler, RPC, ledger, services, replication. |
| `magicblock-version` | Build/version metadata. | none | `magicblock-validator`, `magicblock-aperture` | Keep version reporting stable for RPC/operator tooling. |

## RPC, API, and operator-facing crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-aperture` | Solana-compatible JSON-RPC and websocket/pubsub server. | account cloner, accounts-db, chainlink, config, core, ledger, metrics, version | `magicblock-api` | Handles RPC methods, subscriptions, transaction submission, local read misses/cloning. |
| `magicblock-rpc-client` | RPC client utilities for sending/confirming base-layer transactions. | `magicblock-metrics` | committor, table-mania, account-cloner, API/admin | Critical for base-layer commit delivery. |
| `magicblock-validator-admin` | Admin/client helpers for validator management operations. | `magicblock-program`, `magicblock-rpc-client` | `magicblock-api` | Keep compatible with operator/admin workflows. |
| `magicblock-tui-client` | TUI client/binary support. | none | `magicblock-validator` | UI-facing; should not own core validator logic. |

## Execution and storage crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-processor` | Transaction scheduler, executor pool, SVM execution, commit-to-local-state path. | `magicblock-accounts-db`, `magicblock-core`, `magicblock-ledger`, `magicblock-magic-program-api`, `magicblock-metrics`, `magicblock-program` | `magicblock-api`, tests | Core execution path; preserve account locking and writable-account access invariants. |
| `magicblock-accounts-db` | Custom local account database. | `magicblock-config`, `magicblock-magic-program-api` | account cloner, accounts, aperture, API, chainlink, processor, replicator, tests/tools | Append-only mmap storage plus indexes/snapshots; maintenance requires scheduler pause. |
| `magicblock-ledger` | Local ledger/history and latest block state. | `magicblock-core`, `magicblock-metrics`, `solana-storage-proto`, `test-kit` | aperture, API, processor, replicator, task scheduler, tools/tests | Stores tx/status/block history and latest blockhash/slot. |
| `solana-storage-proto` | Generated/protobuf storage support. | none | `magicblock-ledger` | Low-level ledger serialization support. |

## Delegation, cloning, and account lifecycle crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-chainlink` | Base-chain account/delegation coordination. | accounts-db, AML, config, core, magic-program API, metrics | account-cloner, accounts, aperture, API, magic program | Checks/clones remote accounts, tracks delegation state, coordinates base-layer reads. |
| `magicblock-account-cloner` | Fetches and injects base-layer accounts/programs into local validator state. | accounts-db, chainlink, committor-service, config, core, ledger, magic-program API, magic program, rpc-client | accounts, aperture, API | Distinguishes fee payer, delegated, and undelegated accounts; handles large/program clone paths. |
| `magicblock-accounts` | Account manager and scheduled commit processing glue. | account-cloner, accounts-db, chainlink, committor-service, core, metrics, magic program | `magicblock-api` | Ensures accounts exist for execution and feeds scheduled commits to committor. |
| `magicblock-aml` | External/cached risk-scoring integration. | `magicblock-config`, `magicblock-core` | `magicblock-chainlink` | Cross-cutting policy/risk support for account flows. |

## Commit and base-layer settlement crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-committor-service` | Executes scheduled base-layer intents: commit, undelegate, finalize, action. | committor program, core, metrics, magic program, rpc-client, table-mania | account-cloner, accounts, API | Durable commit pipeline; handles scheduling, transaction prep, buffers, ALTs, confirmations. |
| `magicblock-committor-program` | On-chain committor program. | none | `magicblock-committor-service` | Base-layer program side for changeset buffers/commit application. |
| `magicblock-table-mania` | Address lookup table management. | metrics, rpc-client | `magicblock-committor-service` | Creates/extends/deactivates/closes ALTs needed by commit transactions. |

## Magic Program and shared protocol crates

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-program` | Magic Program implementation (`programs/magicblock`). | chainlink, core, magic-program API, test-kit | account-cloner, accounts, API, committor, processor, task scheduler, admin | Implements scheduling, cloning, ephemeral accounts, validator-only operations. |
| `magicblock-magic-program-api` | Shared Magic Program instruction, PDA, args, and compatibility types. | none | core, accounts-db, chainlink, processor, magic program, services, cloner, API, test programs | Use this instead of duplicating Magic Program wire types. |
| `guinea` | Test-only program for validator behavior. | `magicblock-magic-program-api` | processor tests, test-kit | Used to exercise ephemeral/delegated behavior in tests. |

## Scheduling, replication, services, and observability

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `magicblock-task-scheduler` | Program-scheduled task/crank service. | config, core, ledger, magic program | `magicblock-api` | SQLite-backed delay queue, retries/backoff, scheduled transaction submission. |
| `magicblock-replicator` | Primary/replica event replication over NATS JetStream. | accounts-db, config, core, ledger | `magicblock-api` | Preserves HA/replica replay behavior; primary and replica modes differ intentionally. |
| `magicblock-services` | Shared service utilities/adapters. | core, magic-program API | `magicblock-api` | Common service abstractions; keep generic. |
| `magicblock-metrics` | Metrics helpers and instrumentation. | none | RPC, ledger, processor, chainlink, committor, table-mania, API | Prefer adding observability here rather than ad-hoc metrics code. |

## Tools and test support

| Crate | Purpose | Depends on | Used by | Notes |
|---|---|---|---|---|
| `test-kit` | Shared integration/unit test helpers. | guinea, accounts-db, core, ledger, processor | aperture, committor, ledger, processor, magic program tests | Put reusable test harness logic here. |
| `genx` | Developer/tooling binary. | `magicblock-accounts-db` | manual/tooling use | Keep outside runtime-critical paths. |
| `ledger-stats` | Ledger/accounts statistics tool. | accounts-db, core, ledger | manual/tooling use | Useful for inspecting local persisted state. |
| `keypair-base58` | Keypair conversion/helper binary. | none | manual/tooling use | Small standalone operator/dev helper. |

## How to use this map

- For transaction correctness, start with `magicblock-processor`, then inspect `magicblock-accounts-db`, `magicblock-ledger`, and `magicblock-program` interactions.
- For delegation or account cloning bugs, start with `magicblock-chainlink`, `magicblock-account-cloner`, and `magicblock-accounts`.
- For commit or undelegation bugs, start with `magicblock-program`, `magicblock-accounts`, and `magicblock-committor-service`.
- For RPC behavior, start with `magicblock-aperture`; check `magicblock-chainlink` if reads trigger cloning.
- For validator lifecycle/startup/shutdown, start with `magicblock-api` and `magicblock-validator`.
- When adding, removing, renaming, or repurposing a crate, update this file and `AGENTS.md` in the same change.
