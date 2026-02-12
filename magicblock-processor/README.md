# Magicblock Processor

Core transaction processing engine for the Magicblock validator.

## Overview

This crate is the heart of the validator's execution layer. It provides a high-performance, parallel transaction processing pipeline built around the Solana Virtual Machine (SVM). Its primary responsibility is to take sanitized transactions from the rest of the system, execute or simulate them, commit the resulting state changes, and broadcast the outcomes.

The design is centered around a central **Scheduler** that distributes work to a pool of isolated **Executor** workers, enabling concurrent transaction processing.

## Architecture

```
                    ┌─────────────────────────────────────────┐
                    │         TransactionScheduler            │
                    │  ┌───────────────────────────────────┐  │
  Transactions ───▶ │  │      ExecutionCoordinator         │  │
                    │  │  ┌─────────┐  ┌───────────────┐   │  │
                    │  │  │  Locks  │  │ Blocked Queues│   │  │
                    │  │  │ (u64/   │  │  (BinaryHeap) │   │  │
                    │  │  │ account)│  │               │   │  │
                    │  │  └─────────┘  └───────────────┘   │  │
                    │  └───────────────────────────────────┘  │
                    └──────────────┬──────────────────────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              ▼                    ▼                    ▼
     ┌─────────────┐      ┌─────────────┐      ┌─────────────┐
     │  Executor 0 │      │  Executor 1 │      │  Executor N │
     │  (thread)   │      │  (thread)   │      │  (thread)   │
     └─────────────┘      └─────────────┘      └─────────────┘
```

## Core Components

### TransactionScheduler

The central coordinator and single entry point for all transactions. Runs in a dedicated thread with its own Tokio runtime.

- Receives transactions from external components via MPSC channel
- Dispatches transactions to available executors
- Handles executor readiness notifications
- Manages slot transitions (sysvar updates, program cache pruning)

### ExecutionCoordinator

Manages transaction scheduling and account locking:

- **Locking**: Bitmask-based read/write locks per account (single `u64` per account)
  - Supports up to 63 concurrent executors
  - Multiple readers OR single writer per account
- **Queuing**: Blocked transactions queued behind the blocking executor
  - Min-heap ordering by transaction ID (FIFO)
  - Transactions keep their ID when requeued (older transactions get priority)
- **No fairness blocking**: Transactions only block on actual lock conflicts, not on queued transactions

### TransactionExecutor

The workhorse of the system. Each executor runs in its own dedicated OS thread:

- Loads accounts from AccountsDb
- Executes transactions via SVM
- Commits state changes
- Writes to ledger
- Broadcasts status updates

## Scheduling Strategy

The scheduler uses a simple, deadlock-free approach:

1. **Try all locks**: Attempt to acquire all account locks for a transaction
2. **On conflict**: Release any partial locks, queue behind the blocking executor
3. **On executor ready**: Drain its blocked queue, retry transactions (oldest first)

This design:
- **Prevents deadlocks**: No circular wait conditions possible
- **Allows livelocks**: A transaction may be repeatedly requeued (acceptable trade-off)
- **Maintains FIFO ordering**: Within each executor's queue via min-heap

## Transaction Workflow

1. External component sends `ProcessableTransaction` to scheduler
2. Scheduler assigns to a ready executor
3. Coordinator attempts to acquire account locks:
   - **Success**: Transaction sent to executor for processing
   - **Conflict**: Transaction queued behind blocking executor, original executor released
4. Executor processes transaction via SVM
5. On completion: commits state, writes to ledger, broadcasts status
6. Executor signals ready, scheduler drains its blocked queue

## Performance Considerations

- **Thread isolation**: Scheduler and each executor run in dedicated OS threads
- **Lock efficiency**: Single `u64` bitmask per account (no heap allocations for locks)
- **Shared program cache**: BPF programs compiled once, shared across all executors
- **No contention tracking overhead**: Simplified scheduler removes fairness bookkeeping
