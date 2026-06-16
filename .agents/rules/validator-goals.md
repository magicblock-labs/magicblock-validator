# MagicBlock Validator Goals

This file states the goals that should guide implementation decisions. It intentionally avoids protocol details; use `.agents/specs/validator-specification.md` for exact behavior and `.agents/context/architecture.md` for crate/service interactions.

## Overriding goal: security

Security is the highest-priority goal and overrides every other goal in this document, including performance. The validator custodies and settles real funds; a single security regression can make the validator or its customers lose money. When any other goal conflicts with security, security wins.

No change may, under any circumstances:

- **Relax signer/authority requirements.** Whatever requires a signer today must keep requiring it. Never drop, weaken, or work around signer or authority checks.
- **Let local state drift out of sync with the Solana base layer.** Subscriptions, fetching, delegation-record resolution, and account synchronization must stay at least as correct and stable as they are now.
- **Create attacker-triggerable failure modes.** No race conditions, timing/ordering attacks, validator stalls/deadlocks/hangs, resource exhaustion, or any condition that untrusted input can exploit to corrupt state, bypass validation, or affect settlement.

If a desired change cannot be made without compromising any of the above, do not make it; surface the conflict explicitly instead.

## Primary goal

The validator must provide a Solana-compatible, low-latency execution environment for delegated Solana state, then synchronize that state back to Solana safely through MagicBlock's commit and undelegation flows.

A good change preserves:

1. **Security** — signer/authority enforcement, base-layer sync integrity, and freedom from attacker-triggerable conditions. This is non-negotiable and takes precedence over everything below.
2. **Compatibility** — clients and programs should work like normal Solana where the ER model allows it.
3. **ER correctness** — only accounts that are allowed to change in the ER may change in the ER.
4. **Settlement safety** — commits, undelegations, and base-layer actions must be explicit, durable, and recoverable.
5. **Performance** — critical RPC, account synchronization, scheduling, execution, persistence, and settlement paths must remain low-latency and high-throughput, but never at the cost of security.

## Product goals

### Real-time execution

The validator should support applications that need very low latency and high throughput.

Prefer changes that:

- keep RPC, scheduling, and execution paths lean,
- avoid blocking critical loops,
- preserve parallel execution for unrelated accounts,
- avoid unnecessary allocations, lock contention, I/O, polling, cloning, serialization, and logging in hot paths,
- maintain clear separation between async service work and CPU-bound transaction execution.

Do not accept performance regressions unless there is no viable alternative. If correctness or safety requires a slower path, document the tradeoff, expected impact, and mitigation in the change handoff.

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

## Security goals

Security constraints are mandatory and override performance and convenience. See the overriding goal at the top of this file. In practice:

### Signer and authority enforcement

- Every instruction/operation that requires a signer or specific authority today must keep requiring it. This includes Magic Program operations, ephemeral-account create signers, validator-signed `AcceptScheduleCommits`, commit/undelegation authority, and admin/operator entrypoints.
- Never replace an authenticated path with an unauthenticated one, infer authority from untrusted input, or skip checks on an assumed "fast path."

### Base-layer synchronization integrity

- The validator's correctness depends on local state faithfully tracking Solana. Subscriptions, fetching, delegation-record resolution, slot/commitment handling, and freshness checks must stay at least as strong as today.
- Do not introduce paths where the validator serves or executes against stale, forged, or out-of-sync account state, or where it can miss base-layer updates that affect delegation/undelegation truth.

### Resistance to attacker-triggerable conditions

- Untrusted clients submit transactions and RPC requests; assume inputs are adversarial. No change may add a condition an attacker can trigger to harm the validator or other users.
- Specifically forbidden: race conditions, time-of-check/time-of-use gaps, ordering/timing attacks, validator stalls/deadlocks/hangs, unbounded resource consumption (CPU, memory, disk, subscriptions, locks), and any path letting one user's input affect another user's funds or state.
- Preserve existing atomicity, locking, deduplication, and bounds; they are often security controls, not just performance optimizations.

## Correctness goals

### Account access

The central correctness goal is that ordinary ER execution must not mutate arbitrary Solana state. Writable accounts must satisfy the MagicBlock access model described in `.agents/specs/validator-specification.md`.

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

Do not trade away any security property — signer/authority enforcement, base-layer sync integrity, or resistance to attacker-triggerable conditions — for performance, simplicity, or compatibility. There is no acceptable tradeoff here.

Do not add protocol behavior in an isolated crate without updating the docs and checking the cross-crate architecture.

## Change checklist

Before finishing a feature, bug fix, or refactor, ask:

0. **Security first:** Did I keep every existing signer/authority requirement? Did I keep base-layer synchronization (subscriptions/fetching/delegation resolution) at least as correct and stable? Did I avoid introducing any attacker-triggerable race, timing, stall, deadlock, or resource-exhaustion condition? If any answer is "no," the change must not ship.
1. Did I preserve the delegated/ephemeral writable-account invariant?
2. Did I preserve the distinction between base-layer state, cloned local state, and ER-only state?
3. Did I preserve commit and undelegation lifecycle behavior?
4. Did I preserve restart recovery for any pending or persisted work?
5. Did I avoid blocking critical scheduler/RPC/executor paths?
6. Did I avoid degrading critical-path latency, throughput, memory use, lock contention, and I/O behavior?
7. If a performance tradeoff was unavoidable, did I explicitly call out why, the expected impact, and mitigation?
8. Did I update the relevant file in `.agents/` if behavior changed?
9. Did I update or create agent documentation for any newly discovered durable behavior, workflow, pitfall, invariant, crate responsibility, validation approach, or stale/missing guidance?
