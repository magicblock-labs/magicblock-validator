# MagicBlock Validator Goals

This document states the system goals that should guide changes to the MagicBlock validator. It is written for AI agents and maintainers who need to decide whether a fix preserves the intended behavior of the runtime.

## Primary goal

The validator exists to provide a Solana-compatible, low-latency execution environment for delegated Solana state, then synchronize that state back to Solana safely through MagicBlock's delegation and commit protocols.

A good change should preserve all three properties:

1. **Solana compatibility where expected**: transactions, programs, accounts, RPC methods, and sysvars should look familiar to Solana developers.
2. **ER-specific correctness**: only accounts that are allowed to change in the ER should change in the ER.
3. **Base-layer settlement safety**: commits and undelegations must be explicit, durable, ordered enough for protocol requirements, and recoverable after restarts.

## Product goals

### Real-time execution

The validator should support applications that need much faster interaction loops than normal Solana base-layer finality.

Implications for code changes:

- Keep transaction submission and scheduling paths lean.
- Avoid introducing blocking work into the scheduler, executor, RPC, or event loops.
- Preserve the dedicated execution domains: main async runtime, RPC runtime/thread, and CPU-bound transaction execution thread(s).
- Keep account locks precise; unrelated accounts should be able to execute in parallel.

### Low-cost and gasless-feeling UX

MagicBlock ERs are intended to support zero-fee or low-fee user experiences.

Implications:

- Fee-payer cloning and local lamport handling are intentionally different from base Solana in places.
- Do not assume Solana base-layer fee mechanics apply unchanged inside ER execution.
- Commit fees and delegated payer charging are part of the commit pipeline and should be handled explicitly.

### Solana composability

Delegated accounts should still compose with Solana programs and read-only Solana state.

Implications:

- Program accounts are not delegated; they are cloned as needed and subscribed for updates.
- Undelegated accounts can be cloned for reads, but ordinary ER transactions must not modify undelegated writable state.
- Account/program cloning paths must remain compatible with Solana loader variants.
- RPC compatibility matters because clients should be able to reuse familiar Solana tooling.

### No state fragmentation for delegated accounts

The ER should operate on delegated state and then settle it back to Solana, not create an unrelated fork of application state.

Implications:

- Delegation metadata, delegation records, original owner, delegation slot, and validator authority must remain coherent.
- The validator must clone the right base-layer version before executing delegated state.
- Commit and undelegation must update the base-layer representation through the expected protocol.

### Safe horizontal scaling and HA

The system supports multiple ER endpoints and primary/replica operation.

Implications:

- Replication output and blockhash/slot transitions are part of correctness, not just observability.
- Primary and replica modes intentionally differ: primary can execute in parallel, replica replays strict ordering.
- Streaming blockhash divergence detection is important for finding primary/replica drift.

## Correctness goals

### Account access correctness

The most important execution invariant is:

> Writable accounts in ER execution must be delegated, ephemeral, or otherwise explicitly allowed by MagicBlock's SVM access rules.

The SVM fork enforces this after execution. This is why changes around account flags, account loading, transaction execution, or rollback need extra care.

### Delegation correctness

Delegation is the process of transferring ownership of program state accounts to MagicBlock's Delegation Program on Solana. The ER validator must treat a correctly delegated account as writable local application state, but still remember that base-layer authority is controlled by delegation records and validator assignment.

Goals:

- A delegated account can be cloned into ER on first use.
- A delegated account is locally presented with its original owner so application programs work normally.
- A transaction fails or is rejected if it would mix delegated and undelegated writable state in a way that violates the protocol.
- An account already delegated should not be delegated again.
- An account already undelegated should not be committed/undelegated as if still delegated.

### Commit correctness

Commit is the process of updating base-layer account state from the ER while keeping the account delegated.

Goals:

- Commits must be scheduled explicitly by program instructions or equivalent protocol paths.
- Commit intents should be durable enough to recover after validator restart.
- Commit tasks should be packed into valid base-layer transactions, respecting CPI depth, account limits, address lookup table needs, and large changeset buffering.
- Commit IDs/nonces must remain valid for the delegation program.
- Committed state must correspond to the intended ER account state at the time of scheduling.

### Undelegation correctness

Undelegation commits state and returns account ownership from the Delegation Program to the original program on Solana.

Goals:

- Commit-and-undelegate is not just a commit; it also changes account lifecycle state.
- Once undelegation is scheduled, the ER validator should refuse further ordinary transactions for that account because it is no longer considered delegated to this validator.
- The base-layer callback path must restore ownership correctly.
- The undelegation callback discriminator used by MagicBlock SDK/macros must remain compatible with deployed programs.

### Ephemeral account correctness

Ephemeral accounts exist only inside the ER. They are sponsored by delegated accounts and use MagicBlock's rent model.

Goals:

- Ephemeral accounts can be created, resized, and closed in the ER.
- Sponsors pay/refund rent according to the MagicBlock rent model.
- Ephemeral accounts should not be treated as base-layer delegated accounts.
- The access-control rules should still permit valid ephemeral writes.

### Persistence and recovery

The validator stores local account state, ledger history, task state, and pending commits. Restart behavior is part of the system contract.

Goals:

- AccountsDb and ledger flush/restore paths should remain reliable.
- Pending commit intents should be recovered on startup.
- Task scheduler data should survive restarts where configured.
- Snapshot/defrag/checksum should not race transaction execution.

## RPC/API goals

The validator and router expose Solana-compatible JSON-RPC methods plus MagicBlock-specific functionality.

Goals:

- Standard Solana methods should behave close enough for existing clients and tooling.
- ER-specific methods should expose delegation/routing information, such as delegation status and blockhash-for-accounts behavior.
- Read requests that miss locally may trigger just-in-time cloning from the base layer.
- Websocket subscriptions should receive account, program, signature, logs, and slot updates from validator events.

## Scheduling and task goals

The validator supports two different scheduling concepts:

1. **Transaction scheduling**: internal scheduler that dispatches transactions to executor workers with account locks.
2. **Program task scheduling**: Magic Program task scheduling for future/repeated cranks, persisted through SQLite delay queues and retries.

Goals:

- Transaction scheduling should maximize parallelism without deadlocks.
- Program-scheduled tasks should retry with backoff and not block the scheduler loop indefinitely.
- Failed task records should be retained/cleaned according to configuration.

## Non-goals and cautions

- Do not make the validator behave exactly like a normal Solana validator if doing so breaks ER semantics.
- Do not allow arbitrary writable Solana accounts to mutate in the ER for convenience.
- Do not bypass delegation records or commit nonces to make tests simpler.
- Do not treat commits as immediate local writes to Solana; they are scheduled base-layer intents executed by the committor pipeline.
- Do not remove startup/shutdown recovery behavior unless there is a replacement.

## Validation mindset for fixes

When changing code, ask:

1. Does this preserve the delegated/ephemeral writable-account invariant?
2. Does it distinguish base-layer state, cloned local state, and ER-only state?
3. Does it preserve commit and undelegation lifecycle transitions?
4. Does it preserve restart recovery for pending state transitions?
5. Does it preserve Solana-compatible developer ergonomics where expected?
6. Does it maintain scheduler/executor parallelism and avoid blocking critical loops?
