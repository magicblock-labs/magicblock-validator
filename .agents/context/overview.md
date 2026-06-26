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

## Document routing and policy

Use `.agents/README.md` for document routing; this file only orients on validator concepts.

For goal and security policy, read `.agents/rules/validator-goals.md`. For protocol invariants, read `.agents/specs/validator-specification.md`. For durable-memory and documentation-stewardship rules, read `.agents/memory/agent-memory-and-docs.md`.

## Things to be especially careful with

- Writable account access rules for delegated, ephemeral, confined, and explicitly allowed accounts.
- Delegation and undelegation lifecycle transitions.
- Commit intent durability and restart recovery.
- Account cloning distinctions between delegated, undelegated/read-only, fee-payer, program, and large accounts.
- Scheduler/account-lock correctness and executor parallelism.
- Avoiding avoidable latency, throughput, allocation, lock-contention, or I/O regressions in hot paths.
- Startup/shutdown ordering and persistent store flushing.
