# MagicBlock Validator Overview

This is the shortest orientation document for agents. It explains what this repository is for and points to the files that contain the deeper details. Avoid duplicating detailed protocol or crate architecture here.

## What this validator is

The MagicBlock validator is a specialized Solana Virtual Machine (SVM) runtime for Ephemeral Rollups (ERs). It executes transactions locally against delegated and ephemeral state, while preserving a path for state to be synchronized back to Solana.

In practical terms, the validator:

- accepts Solana-style RPC and transaction traffic,
- clones required base-layer accounts/programs into local state,
- executes transactions with MagicBlock-specific SVM rules,
- persists local account and ledger state,
- schedules commits/undelegations back to the base layer,
- supports task scheduling, replication, metrics, and operator/admin flows.

## Core concepts

| Concept | Short meaning |
|---|---|
| Ephemeral Rollup | A low-latency SVM execution environment attached to Solana. |
| Delegated account | A Solana state account locked by the Delegation Program and assigned to an ER validator for local execution. |
| Ephemeral account | An ER-only account sponsored by delegated state. |
| Commit | Synchronize ER state back to Solana while keeping the account delegated. |
| Commit and undelegate | Synchronize ER state and return ownership to the original program on Solana. |
| Magic Program | ER program used for commit scheduling, intent bundles, tasks, ephemeral accounts, and validator operations. |
| Committor | Validator-side service that realizes scheduled base-layer intents. |

## Which agent doc to read

- `agents/01_validator-goals.md` — read when deciding whether a change is aligned with product/system goals.
- `agents/02_specification.md` — read before changing protocol behavior: delegation, cloning, execution rules, commits, undelegation, Magic Actions, ephemeral accounts, RPC/router behavior, or recovery.
- `agents/03_architecture.md` — read before changing service wiring or interactions between crates.
- `agents/04_crate-map.md` — read to find which crate owns an area and which other crates may be affected.
- `agents/05_testing-and-validation.md` — read before deciding how to validate a change.
- `agents/06_agent-memory-and-docs.md` — read when a task may discover or change durable repository knowledge that future agents should remember.

## Non-negotiable agent rules

Before changing behavior, check the relevant agent docs and preserve the validator's goals, invariants, and performance expectations.

The validator is performance-sensitive infrastructure. Do not degrade critical RPC, account sync, scheduling, execution, persistence, or settlement paths unless there is no viable alternative; if such a tradeoff is unavoidable, state it explicitly with expected impact and mitigation.

When behavior changes, update the relevant `agents/` file in the same change. These docs must remain synchronized with the real implementation; stale guidance is worse than no guidance.

When you discover durable knowledge that is missing or wrong in `./agents/`—for example a feature behavior, protocol invariant, crate responsibility, validation workflow, recurring pitfall, or performance constraint—update the most relevant existing document, or create a focused new one if no suitable document exists. If you cannot make the documentation update, report the exact follow-up before handing off.

## Things to be especially careful with

- Writable account access rules for delegated, ephemeral, confined, and explicitly allowed accounts.
- Delegation and undelegation lifecycle transitions.
- Commit intent durability and restart recovery.
- Account cloning distinctions between delegated, undelegated/read-only, fee-payer, program, and large accounts.
- Scheduler/account-lock correctness and executor parallelism.
- Avoiding avoidable latency, throughput, allocation, lock-contention, or I/O regressions in hot paths.
- Startup/shutdown ordering and persistent store flushing.
