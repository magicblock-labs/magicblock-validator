# Correctness Invariants: MagicBlock Validator and Runtime

## Purpose

This document defines the correctness invariants that must always hold in the MagicBlock validator and runtime.

The goal is to describe stable system guarantees, not implementation details. File paths, line numbers, function names, discriminants, and current code structure are intentionally omitted because they may change.

Anything that breaks one of these invariants must be detected before it can corrupt protected state, lose user-visible effects, or create divergence between validators.

## 1. Account Protection Invariants

The ephemeral rollup is only sound if local writes are restricted to accounts the validator is entitled to mutate, and remote base-chain state cannot overwrite locally protected state.

### 1.1 Writable Access Must Be Authorized

A transaction may write only accounts that the validator is authorized to mutate.

A writable account must be one of the following:

- Delegated
- Ephemeral
- Confined
- Undelegating, subject to stricter undelegation rules

Any transaction that writes an unauthorized account must fail before its state changes are committed.

Fee-payer lamport changes are allowed only when the fee payer is delegated or the transaction is explicitly privileged. Otherwise, the transaction must fail and no invalid state change may be committed.

Privileged access must remain tightly scoped to validator-controlled flows. It must not become a general bypass for user transactions.

Undelegating accounts are locked. They must not be locally mutated. The only permitted transition is confirmed undelegation completion from the base chain. Once undelegation completes, the account becomes non-delegated and must not be written locally again.

**Invariant:** A transaction that writes an unauthorized account must fail before commit.

### 1.2 Remote Updates Must Not Override Protected Local State

While an account is delegated, the local validator is the source of truth for that account.

Remote base-chain updates must not overwrite local delegated state.

An undelegating account must also remain protected from remote overwrite until undelegation is confirmed on chain.

Remote updates must be monotonic by slot. Stale or out-of-order updates must not regress local state. Same-slot updates may only add delegation information and must not overwrite local delegated data.

**Invariant:** Delegated and undelegating account state may only move forward according to local execution, except after confirmed undelegation.

### 1.3 Protected Accounts Must Not Be Evicted

Delegated and undelegating accounts must not be removed by cache eviction, capacity eviction, or LRU eviction.

Eviction logic must never convert a protected account into a local eviction transaction.

**Invariant:** Accounts that are still locally protected must remain present, protected, and observable.

### 1.4 Protection Flags Must Be Validator-Controlled

Account protection flags must not be user-controlled.

This includes:

- Delegated
- Ephemeral
- Undelegating
- Confined
- Privileged

These flags may only be changed by validator-authorized flows or by trusted reflection of base-chain delegation state.

**Invariant:** User transactions must not be able to change account protection flags.

### 1.5 Confined Accounts Must Remain Local

Confined accounts are local runtime scratch space.

They must not be committed to the base chain, undelegated, resized, or used to move lamports in or out.

**Invariant:** Confined accounts must remain local-only scratch space and must not leak into base-chain state.

### 1.6 Post-Delegation Actions Must Execute Exactly Once

A post-delegation action is associated with exactly one delegated account.

It must execute once and only once per delegation.

This depends on delegated state not being overwritten or re-cloned by remote updates.

**Invariant:** Each delegation may trigger at most one post-delegation action, and the intended action must eventually execute.

### 1.7 ATA Projection Must Use Valid Delegated eATA State

An ATA may be projected into local delegated form only when the corresponding eATA exists on chain, is valid, and is delegated to this validator.

Projected ATA state must be derived from the eATA balance and the base ATA layout.

A delegated projected ATA must commit back to its eATA, not to the base ATA.

**Invariant:** Projected token-account state must be backed by valid delegated eATA authority and must commit only to the delegated eATA target.

### 1.8 Delegation State Must Be Slot-Consistent

An account and its delegation record must be resolved from the same base-chain slot.

Delegation flags must not be applied using a delegation record from a different slot than the account snapshot.

**Invariant:** Account state and delegation metadata must be resolved consistently from the same slot.

### 1.9 Protected Accounts Must Remain Observable

Delegated and undelegating accounts must retain the subscriptions or tracking needed to observe their base-chain state.

The validator must not silently stop monitoring an account that is still protected or awaiting undelegation confirmation.

**Invariant:** Undelegation completion must always remain observable.

### 1.10 Accounts Delegated to Another Validator Must Be Read-Only

If an account is delegated to another validator, this validator must treat it as non-delegated.

It must not accept local writes to that account.

It must not produce commits for that account.

**Invariant:** Validator authority boundaries must be enforced for both execution and commit.

## 2. Input Invariants

Transactions entering execution must be valid, non-replayed, and backed by a currently valid blockhash.

Before scheduling:

- All referenced accounts and programs must exist locally or be fetched successfully.
- Writable accounts must have their delegation status resolved.
- Replay protection must cover the full blockhash validity window.
- Clone operations must respect account-size limits.
- Large accounts must complete the full chunked clone sequence before becoming usable.
- Commit intents must enter through an authorized program path.
- Commit intents must explicitly include the delegated accounts they affect.
- Staged intent data must fit within the allowed runtime context size.

**Invariant:** No transaction may enter execution unless its accounts, authority, replay status, and blockhash validity have already been resolved.

## 3. Output Invariants

Every scheduled transaction must produce exactly one final transaction status.

A successful transaction must also produce account-update events that match the committed account state.

For each transaction, the following must remain consistent:

- Transaction status
- Logs
- Ledger entries
- Persistent storage
- AccountsDb state

Failed transactions must still produce status and logs.

Failed transaction account changes must roll back, except for valid fee-payer and nonce effects allowed by the runtime.

Commit intents must eventually produce one of the following:

- A confirmed base-chain transaction
- A reported failure

Pending commit intents must survive validator restarts and must not be silently dropped.

**Invariant:** Every accepted transaction or intent must have a durable, queryable outcome.

## 4. Determinism Invariants

Blockhashes must be deterministic functions of execution history.

Two validators that execute the same transactions in the same order must produce the same account state and blockhashes.

Replay from a snapshot and ledger must reproduce the same protected account state and blockhash chain.

Only the scheduler-chosen serialization order may influence committed state.

The following must not affect committed state:

- Lock acquisition timing
- Executor assignment
- Thread timing
- Wall-clock timing
- Other non-deterministic runtime details

Any difference in transaction content or execution order must change the blockhash.

A replicated Block must not be persisted before every preceding transaction is
persisted. At checkpoint slots, the stream order must be Transaction, Block,
then SuperBlock; next-slot work must not overtake that checkpoint.

Protected account checksums must change whenever protected state changes.

Replica failover must continue the existing blockhash chain. Divergence must be detected by blockhash or checksum mismatch.

**Invariant:** Identical ordered inputs must produce identical outputs, and differing ordered inputs must be detectable.

## 5. Pre-State-Change Invariants

For user transactions, checks must occur before commit in this logical order:

1. Signature and transaction validation
2. Replay protection
3. Blockhash validity
4. Account and program availability
5. Delegation resolution
6. Fee-payer and nonce validation
7. All-or-nothing account lock acquisition
8. Execution
9. Post-execution access validation
10. Commit

Account locks must be acquired atomically.

Partial lock acquisition must not leave residual locks.

Maintenance operations require exclusive access. This includes:

- Snapshotting
- Defragmentation
- Checksum generation

These operations must only run while the scheduler is quiesced.

**Invariant:** No state-changing operation may bypass validation, lock discipline, or exclusive-access requirements.

## 6. Error Invariants

Invalid inputs must fail predictably.

The system must enforce the following behavior:

- Unauthorized writes must fail and discard state changes.
- Unauthorized fee-payer lamport changes must fail.
- Invalid fee-payer balance must fail according to runtime rules.
- Invalid nonce state must fail according to runtime rules.
- Invalid account loading must fail according to runtime rules.
- Duplicate signatures within the replay window must be rejected.
- Missing accounts that cannot be fetched must fail before scheduling.
- Privileged instructions that do not meet privilege requirements must fall back to normal writability checks and fail if unauthorized.

**Invariant:** Invalid inputs must fail deterministically, visibly, and before corrupting state.

## 7. Commit and Intent Lifecycle Invariants

### 7.1 Commit Nonces Must Strictly Advance

Every base-chain commit must use the next valid nonce for the delegated account.

Out-of-order commits must be rejected.

Replayed commits must be rejected.

Retries must be idempotent. A retry must not re-apply an already-landed commit.

**Invariant:** Commits must be ordered, replay-safe, and idempotent.

### 7.2 Commit Must Precede Undelegation

An account must not be undelegated on the base chain until its latest local state has been committed.

Finalize or undelegate actions may only occur after the relevant commit is confirmed.

**Invariant:** Undelegation must not discard uncommitted local state.

### 7.3 Intent Execution Must Be Serialized Per Account

Two intents touching the same account must not execute concurrently.

Intents for the same account must execute in intent order.

The same intent must not be scheduled twice.

**Invariant:** Per-account commit order must be deterministic and non-duplicated.

### 7.4 Bundled Commits Must Be Atomic

Accounts staged together in one scheduled commit form one logical bundle.

A bundle must commit atomically. Either all accounts in the bundle are committed, or the bundle is treated as failed.

If a bundle must be split across multiple base-chain transactions, the system must still preserve the logical atomicity of the original bundle.

**Invariant:** Scheduled commit bundles must not partially succeed in a way that becomes visible as final state.

### 7.5 Commit Buffers Must Be Commit-Scoped

Commit buffers and chunks must be derived from the commit identity and must be fresh for each commit.

Old buffer data must never be reused as current state.

**Invariant:** Stale commit data must not be deliverable as a fresh commit.

### 7.6 Only Validator Authority May Drive Commit Flow

Accepting scheduled commits, submitting base-layer commits, finalizing, and undelegating must all be authorized by the effective validator authority.

**Invariant:** Commit authority must remain single, explicit, and consistent.

### 7.7 Intent Acceptance Must Be Atomic

Draining staged intents and handing them to the committor must be one atomic operation.

An intent must never be both retained in staging and enqueued for execution.

An intent must never be enqueued twice.

The scheduler handoff and staging-context update must succeed or fail together.

**Invariant:** Intent handoff must be exactly once.

## 8. Scheduler, Storage, and RPC Invariants

### 8.1 Lock Lifecycle Must Be Complete

Every account lock acquired for a transaction must be released exactly once.

This must hold on both success and failure paths.

An executor must not return to the ready pool while still holding locks.

**Invariant:** Locks must not leak, duplicate, or outlive the transaction that acquired them.

### 8.2 Scheduled Transactions Must Always Be Delivered

A transaction that has acquired locks and claimed an executor must either reach that executor or have all claimed resources reclaimed.

If delivery fails:

- The transaction's locks must be reclaimed.
- The executor slot must be reclaimed.
- The transaction must not be silently dropped.
- The executor must not be lost from the pool.

**Invariant:** Scheduling failure must be recoverable without losing resources or transactions.

### 8.3 Slot Indices Must Be Dense and Unique

Within each slot, executed transactions must receive strictly increasing indices starting at zero.

The pair of slot and index must uniquely identify a ledger entry.

**Invariant:** Ledger ordering must be total, dense, and replayable.

### 8.4 Ledger Persistence Must Precede Slot Advancement

A slot must not be marked current in AccountsDb until the corresponding block has been persisted to the ledger.

**Invariant:** AccountsDb must never run ahead of durable ledger state.

### 8.5 Fees Must Be Charged Exactly Once

Each executed transaction must apply commit accounting exactly once.

Fee behavior must be consistent:

- Load-stage failures charge no fees.
- Executed failed transactions charge the fee payer exactly once.
- Fees-only transactions charge the fee payer exactly once.

**Invariant:** Fees must be neither skipped nor duplicated.

### 8.6 Truncation Must Be Safe and Monotonic

The cleanup boundary may only move forward.

The latest slot must never be truncated.

Reads below the cleanup boundary must be rejected.

**Invariant:** Truncation must not remove required live history or create ambiguous reads.

### 8.7 Replay Cache Must Only Record Scheduled Transactions

A signature must enter replay protection only after the transaction has actually been handed to the scheduler.

A transaction rejected before scheduling must remain immediately re-submittable.

**Invariant:** Replay protection must not reject transactions that never entered execution.

### 8.8 Signature Subscriptions Must Deliver At Least Once

A signature subscriber must receive the final transaction status whether it subscribes before or after execution completes.

There must be no window where the result is known but neither cached nor delivered.

**Invariant:** Final transaction status must not be lost.

## 9. Required Invariants to Harden Existing Assumptions

The following assumptions are relied on by the system and should be enforced, tested, or documented as hard invariants.

### 9.1 Maintenance Requires Enforced Exclusive Access

Snapshot, defragmentation, and checksum operations assume exclusive scheduler access.

**Required invariant:** Exclusive access must be enforced by the system, not only assumed by call-site convention.

### 9.2 Storage Layout Must Be Versioned

In-memory, mmap, and account-borrowing layouts must agree on byte-level structure.

This includes:

- Lamports layout
- Owner layout
- Flags layout
- Data layout
- Shadow-buffer layout

**Required invariant:** Layout changes must be guarded by versioning, ABI checks, or equivalent compatibility protection.

### 9.3 Executor Limits Must Be Enforced

The account-lock representation assumes a maximum executor count.

**Required invariant:** Configured executor count must be validated against lock representation limits.

### 9.4 Unsafe Concurrency Contracts Must Be Enforced

Cross-thread safety depends on internal synchronization assumptions.

These assumptions must not live only in comments.

**Required invariant:** Synchronization assumptions must be tested, documented, and guarded by runtime or type-level checks where possible.

### 9.5 Storage Must Have a Single Writer Per Account

The storage layer assumes account-level write serialization.

This is currently guaranteed by scheduler discipline, but the storage layer should not rely only on external correctness.

**Required invariant:** Storage writes must enforce or validate single-writer access per account.

### 9.6 Privileged Instruction Definitions Must Stay Synchronized

Runtime privileged-instruction allowlists and magic-program instruction definitions must remain consistent.

**Required invariant:** Privileged instruction compatibility must be enforced through shared definitions, generated constants, or compatibility tests.

### 9.7 Commit Acceptance Must Remain Live

Staged commit intents assume regular acceptance and bounded context growth.

**Required invariant:** Staged intents must not grow without progress and must not overflow context capacity.

### 9.8 Serialized Schemas Must Be Versioned

Persisted ledger data and replication streams assume stable serialization schemas.

**Required invariant:** Persisted and wire-format schemas must include versioning or compatibility protection.
