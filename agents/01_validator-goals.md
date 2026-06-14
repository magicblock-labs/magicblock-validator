# MagicBlock Validator Goals

This file states the goals that should guide implementation decisions. It intentionally avoids protocol details; use `agents/02_specification.md` for exact behavior and `agents/03_architecture.md` for crate/service interactions.

## Primary goal

The validator must provide a Solana-compatible, low-latency execution environment for delegated Solana state, then synchronize that state back to Solana safely through MagicBlock's commit and undelegation flows.

A good change preserves:

1. **Compatibility** — clients and programs should work like normal Solana where the ER model allows it.
2. **ER correctness** — only accounts that are allowed to change in the ER may change in the ER.
3. **Settlement safety** — commits, undelegations, and base-layer actions must be explicit, durable, and recoverable.

## Product goals

### Real-time execution

The validator should support applications that need very low latency and high throughput.

Prefer changes that:

- keep RPC, scheduling, and execution paths lean,
- avoid blocking critical loops,
- preserve parallel execution for unrelated accounts,
- maintain clear separation between async service work and CPU-bound transaction execution.

### Low-cost user experience

The ER is expected to support gasless-feeling or low-cost application flows.

Prefer changes that:

- do not blindly import base-layer fee assumptions into ER execution,
- preserve fee-payer and sponsor behavior where the ER intentionally differs from Solana,
- keep commit/settlement fees explicit in the commit pipeline.

### Solana composability

Delegated state should still compose with Solana programs and read-only Solana state.

Prefer changes that:

- keep standard program execution familiar,
- preserve read access to cloned base-layer accounts/programs,
- avoid fragmenting application state into incompatible ER-only copies unless the account is explicitly ephemeral.

### Safe settlement

Local ER state must have a safe path back to the base layer.

Prefer changes that:

- keep commit and undelegation scheduling explicit,
- preserve commit ordering/nonce requirements,
- recover pending settlement work after restart,
- do not make local success look like base-layer settlement before the settlement pipeline has actually run.

### High availability and observability

The validator should remain operable in production-like deployments.

Prefer changes that:

- keep replication semantics intentional,
- make failures observable,
- avoid hiding commit, clone, scheduler, or RPC errors,
- preserve metrics/admin/operator visibility.

## Correctness goals

### Account access

The central correctness goal is that ordinary ER execution must not mutate arbitrary Solana state. Writable accounts must satisfy the MagicBlock access model described in `agents/02_specification.md`.

### Lifecycle transitions

Delegation, commit, commit-and-undelegate, and ephemeral account lifecycle transitions must remain coherent across:

- local account flags/representation,
- SVM execution,
- Magic Program scheduling,
- committor service execution,
- restart recovery.

### Persistence and recovery

Persistent state is part of the system contract. Changes must not break recovery for:

- local account state,
- ledger/history,
- pending commit intents,
- scheduled tasks,
- replica/primary state where applicable.

## Non-goals

Do not optimize for making the validator identical to a normal Solana validator when that conflicts with ER semantics.

Do not bypass delegation records, access validation, commit nonces, or intent persistence to simplify implementation.

Do not add protocol behavior in an isolated crate without updating the docs and checking the cross-crate architecture.

## Change checklist

Before finishing a feature, bug fix, or refactor, ask:

1. Did I preserve the delegated/ephemeral writable-account invariant?
2. Did I preserve the distinction between base-layer state, cloned local state, and ER-only state?
3. Did I preserve commit and undelegation lifecycle behavior?
4. Did I preserve restart recovery for any pending or persisted work?
5. Did I avoid blocking critical scheduler/RPC/executor paths?
6. Did I update the relevant file in `agents/` if behavior changed?
