# MagicBlock Validator Overview

This document is an AI-agent oriented overview of the MagicBlock validator codebase. It summarizes what the validator is for, which product concepts it implements, and which invariants should stay in mind when applying fixes.

## What this validator does

The MagicBlock validator is a specialized Solana Virtual Machine (SVM) runtime for **Ephemeral Rollups (ERs)**. It lets applications move selected state accounts from the Solana base layer into a low-latency auxiliary execution environment, run transactions against those accounts with ER performance, and later synchronize the resulting state back to the base layer.

At a high level the validator:

1. Tracks which accounts are delegated to a specific ER validator.
2. Clones required accounts and programs from the Solana base layer into local validator state.
3. Executes transactions locally with a forked SVM.
4. Enforces MagicBlock-specific account access rules.
5. Stores local account, block, and transaction history.
6. Emits RPC, websocket, Geyser, and replication events.
7. Schedules and realizes commits/undelegations back to the base layer.

The validator is not a generic Solana validator. It is an execution runtime for delegated state, ephemeral accounts, scheduled tasks, and base-layer commit workflows.

## Product model

MagicBlock Ephemeral Rollups are designed for real-time Solana applications that need lower latency, lower cost, and high-throughput execution while preserving Solana composability.

Core ideas:

- **Delegated accounts** are Solana accounts whose ownership is locked on the base layer by the Delegation Program and assigned to an ER validator for local execution.
- **Ephemeral accounts** are ER-only accounts that are born, resized, and closed inside the ER. They are sponsored by a delegated account and use the MagicBlock rent model.
- **Commit** synchronizes ER state back to the base layer while the account remains delegated.
- **Commit and undelegate** synchronizes ER state back to the base layer and returns ownership from the Delegation Program to the original program.
- **Magic Actions** attach base-layer instructions that run after an ER commit, using the freshly committed state.
- **Magic Router** can inspect transaction metadata and route transactions to either Solana or the ER based on account delegation status.

## Account lifecycle

The normal lifecycle for state that moves through an ER is:

1. **Delegate on the base layer**
   - A program-specific delegation hook CPIs into MagicBlock's Delegation Program.
   - The account owner on Solana becomes the Delegation Program.
   - Delegation records identify the selected ER validator and original owner.
   - No ER account necessarily exists yet.
2. **Clone into the ER on demand**
   - The first ER read or transaction involving the account causes the validator to fetch base-layer state and delegation metadata.
   - Delegated accounts are materialized locally with their original owner so application programs can use them normally in SVM execution.
   - Read-only undelegated accounts and programs may also be cloned for composability, but they must not be written by ordinary ER transactions.
3. **Execute transactions locally**
   - Transactions whose writable accounts are delegated, ephemeral, or otherwise allowed by the forked SVM can execute in the ER.
   - Local writes go to the validator's AccountsDb and transaction/block history goes to the ledger.
4. **Commit**
   - The program schedules a commit intent through the Magic Program.
   - The validator accepts and processes the intent, builds base-layer transactions, and sends them through the committor service.
   - The base-layer account remains delegated.
5. **Undelegate**
   - The program schedules a commit-and-undelegate intent through the Magic Program.
   - The ER account is marked undelegating/immutable locally.
   - The committor realizes the state update and undelegation on Solana.
   - The account ownership returns to the original program after finalization/callback handling.

## Main runtime subsystems

| Subsystem | Primary crates | Role |
|---|---|---|
| Validator orchestration | `magicblock-validator`, `magicblock-api` | Build configuration, initialize services, run RPC/TUI/headless loop, handle startup and shutdown. |
| RPC and subscriptions | `magicblock-aperture` | Solana-compatible JSON-RPC and websocket pubsub, plus MagicBlock-specific RPC behavior. |
| Transaction execution | `magicblock-processor`, forked `magicblock-svm` | Scheduler, account locking, executor workers, SVM execution, status/account events. |
| Local account storage | `magicblock-accounts-db` | Append-only mmap account log, LMDB indexes, snapshots, defrag/checksum. |
| Ledger/history | `magicblock-ledger` | RocksDB transaction/status/block history and latest blockhash/slot state. |
| Base-chain account sync | `magicblock-chainlink`, `magicblock-account-cloner`, `magicblock-accounts` | Clone, validate, and track remote accounts and delegation state. |
| Commit pipeline | `programs/magicblock`, `magicblock-committor-service`, `magicblock-committor-program`, `magicblock-table-mania`, `magicblock-rpc-client` | Schedule ER intents and realize base-layer commits, undelegations, and post-commit actions. |
| Scheduled tasks | `magicblock-task-scheduler`, `programs/magicblock` | Persist and execute program-scheduled cranks/tasks. |
| Replication | `magicblock-replicator` | Primary/replica streaming over NATS JetStream. |

## Execution model in one paragraph

Clients submit transactions through RPC or a router. The validator ensures transaction accounts are present locally, cloning from the base layer when needed. The transaction scheduler acquires all required account locks atomically, dispatches to executor threads, runs the forked SVM, validates account access, commits resulting account changes to AccountsDb, records status/history in the ledger, emits events, and advances local slots/blockhashes. Commit-related instructions do not directly mutate Solana; they stage base-layer intents in MagicContext for the validator's commit pipeline.

## Things AI agents must not break

- Writable ER accounts must satisfy the delegated/ephemeral/confined access rules enforced by the SVM fork.
- Delegated accounts should behave locally as if owned by their original program, while base-layer ownership remains controlled by the Delegation Program.
- Undelegating accounts must become locally immutable/refused for further ordinary transactions once undelegation is scheduled.
- Commit scheduling is intentionally staged; do not collapse Magic Program scheduling and validator acceptance without understanding slot-boundary behavior.
- Account cloning must distinguish delegated, undelegated read-only, and fee-payer accounts.
- Commit/undelegation must survive validator restarts via persisted committor state.
- Maintenance operations such as snapshot, checksum, and defrag require scheduler pausing.
- Shutdown ordering matters: cancellation first, services joined, committor protected for in-flight intents, then databases flushed.
