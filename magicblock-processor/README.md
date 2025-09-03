# Magicblock Processor

Core transaction processing engine for the Magicblock validator.

## Overview

This crate is the heart of the validator's execution layer. It provides a high-performance, parallel transaction processing pipeline built around the Solana Virtual Machine (SVM). Its primary responsibility is to take sanitized transactions from the rest of the system (e.g., the RPC gateway), execute or simulate them, commit the resulting state changes, and broadcast the outcomes.

The design is centered around a central **Scheduler** that distributes work to a pool of isolated **Executor** workers, enabling concurrent transaction processing.

## Core Concepts

The architecture is designed for performance and clear separation of concerns, revolving around a few key components:

-   **`TransactionScheduler`**: The central coordinator and single entry point for all transactions. It receives transactions from a global queue and dispatches them to available `TransactionExecutor` workers.
-   **`TransactionExecutor`**: The workhorse of the system. Each executor runs in its own dedicated OS thread with a private Tokio runtime. It is responsible for the entire lifecycle of a single transaction: loading accounts, executing with the SVM, committing state changes to the `AccountsDb` and `Ledger`, and broadcasting the results.
-   **`TransactionSchedulerState`**: A shared context object that acts as a dependency container. It holds `Arc` handles to global state (like `AccountsDb` and `Ledger`) and the communication channels required for the scheduler and executors to operate.
-   **`link` function**: A helper method that creates the paired MPSC and Flume channels connecting the processor to the rest of the validator (the "dispatch" side). This decouples the processing core from the API/gateway layer.

---

## Transaction Workflow

A typical transaction flows through the system as follows:

1.  An external component (e.g., an RPC handler) receives a transaction.
2.  It calls a method on the `TransactionSchedulerHandle` (e.g., `execute` or `simulate`).
3.  The handle sends a `ProcessableTransaction` message to the `TransactionScheduler` over a multi-producer, single-consumer channel.
4.  The `TransactionScheduler` receives the message and forwards it to an available `TransactionExecutor` worker.
5.  The `TransactionExecutor` processes the transaction using the Solana SVM.
6.  If the transaction is not a simulation:
    -   The executor commits modified account states to the `AccountsDb`.
    -   It writes the transaction and its metadata to the `Ledger`.
    -   It forwards a `TransactionStatus` update and any `AccountUpdate` notifications over global channels.
7.  The `TransactionExecutor` signals its readiness back to the `TransactionScheduler` to receive more work.

## Performance Considerations

The processor is designed with several key performance optimizations:

-   **Thread Isolation**: The scheduler and each executor run in dedicated OS threads to prevent contention and leverage multi-core CPUs.
-   **Dedicated Runtimes**: Each thread manages its own single-threaded Tokio runtime. This provides concurrency for CPU-bound tasks without interfering with the multi-threaded, work-stealing scheduler.
-   **Shared Program Cache**: All `TransactionExecutor` instances share a single, global `ProgramCache`. This ensures that a BPF program is loaded and compiled only once, with the result being immediately available to all workers.

