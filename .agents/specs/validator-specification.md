# MagicBlock Validator Specification Notes

This document captures specification-level behavior that AI .agents should understand before modifying the validator. Treat it as a working spec for fixes, not as a complete formal protocol document.

## Security invariants (highest priority)

These invariants override all other behavior described in this document. The validator handles real funds; violating any of them can cause the validator or its customers to lose money. Under no circumstances may a change weaken them. Other docs should link here for protocol-binding invariants instead of restating them.

1. **Signers stay required.** Every signature/authority check that exists today must remain. This includes (non-exhaustively): the MagicContext payer signer on `ScheduleIntentBundle`, the validator-signed `AcceptScheduleCommits`, ephemeral-account create signer (required on create to prevent pubkey squatting), delegation/commit/undelegation authorities, Magic Action escrow authority, and admin/operator entrypoints. Never add an unsigned or weaker-authority path to an operation that is authenticated today.
2. **Local state stays in sync with the base layer.** Account fetching, websocket/gRPC subscriptions, delegation-record resolution, slot/`min_context_slot` and commitment handling, and clone-freshness checks must remain at least as strong and stable as the current implementation. The validator must not serve or execute against stale, forged, or out-of-sync state, and must not miss base-layer updates that change delegation/undelegation truth.
3. **No attacker-triggerable conditions.** All RPC and transaction inputs are untrusted and potentially adversarial. Do not introduce race conditions, time-of-check/time-of-use gaps, ordering/timing attacks, validator stalls/deadlocks/hangs, unbounded resource consumption, or any path where one user's input can corrupt state, bypass validation, or affect another user's funds. Existing atomic lock acquisition, deduplication, slot-matching, and bounded capacity are security controls — preserve them.

If a change cannot satisfy all three, do not make it; surface the conflict explicitly. The sections below describe specific mechanisms; read them through the lens of these invariants.

## Terminology

| Term | Meaning |
|---|---|
| Base layer | Solana mainnet/devnet/local validator where original accounts and programs live. |
| ER | Ephemeral Rollup: a MagicBlock SVM execution runtime operated by an ER validator. |
| Delegation Program | Base-layer program `DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh` that locks delegated account ownership and stores delegation metadata. |
| Magic Program | ER-local program `programs/magicblock` used for commits, undelegations, scheduled tasks, ephemeral accounts, cloning, and validator-only operations. |
| MagicContext | Account used by the Magic Program to stage scheduled base-layer intents. |
| Delegated account | A base-layer state account whose ownership is locked by the Delegation Program and assigned to an ER validator. Locally it is cloned and presented with its original owner. |
| Undelegated account | A normal Solana account not delegated to this ER. It may be cloned for reads but must not be mutated by ordinary ER execution. |
| Ephemeral account | An ER-only account sponsored by a delegated account; it can be created/resized/closed inside the ER. |
| Commit | Synchronize ER state back to the base layer while keeping account delegation active. |
| Commit and undelegate | Synchronize ER state back to the base layer and return account ownership to the original program. |
| Magic Action | A base-layer instruction/action attached to a commit and run after committed state is available. |

## Delegation specification

### What delegation means

Delegation transfers ownership of one or more program PDAs/state accounts to MagicBlock's Delegation Program on the base layer. The account becomes locked on Solana and associated with an ER validator. The original owner is recorded so the ER can execute the original program against the delegated state.

Important properties:

- Delegation is performed on the base layer, usually by a program-specific delegation hook.
- The ER account is not necessarily created at delegation time.
- Delegated accounts must already exist on Solana before delegation.
- Program accounts are not delegated. Program accounts are cloned when needed.
- A delegation attempt should fail if the account is already delegated.
- Public docs describe delegation configuration as including the ER validator, account lifetime, and synchronization frequency.

### Validator assignment

Delegation should identify the ER validator. MagicBlock public docs list development validators by region and show that programs may pass a specific validator account in delegation config. Local development uses validator identity `mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev` for `localhost:7799`.

The router/API can expose delegation status for a single account via `getDelegationStatus`, returning at least:

- `isDelegated`
- `fqdn` when known
- `delegationRecord.authority`
- `delegationRecord.owner`
- `delegationRecord.delegationSlot`
- `delegationRecord.lamports`

### Local representation

When a delegated account is cloned into the ER:

- The validator fetches account data and delegation metadata from the base layer.
- The account is installed into local AccountsDb.
- The account is presented to the local SVM with its original owner so application programs can use it normally.
- Delegation-related flags/metadata must remain available for access validation and lifecycle handling.

## Account cloning specification

### Just-in-time cloning

The validator clones accounts when transactions or RPC reads need accounts that are not present or are stale locally.

Triggers include:

- A transaction submitted to the validator.
- A read request that misses local AccountsDb.
- Remote account update notifications that indicate cached clone state may be stale.

### Account flavors

The account cloner distinguishes these important flavors:

| Flavor | Meaning | ER behavior |
|---|---|---|
| Fee payer | On-curve system account with no data and not properly delegated. | Can be used to pay transaction costs; cloner may special-case lamports. |
| Undelegated | Non-delegated account with data. | May be cloned/read; should not be written by ordinary ER transactions. |
| Delegated | Account with a valid delegation record. | May be cloned and locally modified if assigned/valid for this ER. |

### Cloning freshness

The cloner tracks remote updates and clone outputs. If a remote account has changed since the last clone, the next clone request should fetch a newer base-layer version. Fetching uses base-layer RPC and delegation record lookups; websocket subscriptions track future changes.

### Large accounts and programs

The Magic Program API includes validator-only clone instructions:

- `CloneAccount` for accounts that fit in one transaction.
- `CloneAccountInit` / `CloneAccountContinue` / `CleanupPartialClone` for large accounts.

Program accounts are cloned/redeployed locally via loader-specific paths. Program accounts are not delegated.

## Transaction routing and execution specification

### Routing model

MagicBlock's product docs specify the routing behavior for ordinary program instructions:

- If **all writable accounts are delegated**, execute on the ER.
  - Newly delegated accounts are cloned from the base layer on first ER use.
  - Already cloned delegated accounts are reused.
  - Undelegated non-writable accounts may be cloned for reads.
- If **all writable accounts are undelegated**, execute on the base layer.
- If a transaction mixes delegated and undelegated writable accounts, it fails or should not be routed to ER execution.

Inside this repository, RPC/router-equivalent paths must preserve the same effective invariant even if routing is performed outside the validator.

### Scheduler and locking

The transaction scheduler runs on a dedicated OS thread and uses a pool of executor workers. This is a critical performance path: preserve low-latency scheduling, bounded lock contention, and parallel execution for unrelated accounts. Do not add blocking I/O, unbounded work, excessive logging, or avoidable allocation/serialization to scheduler or executor hot paths unless there is no viable alternative, and explicitly document any unavoidable tradeoff.

This path is also security-critical. Atomic, all-or-nothing account lock acquisition (step 3 below) is what prevents concurrent transactions from racing on the same accounts; it must never be relaxed into partial or non-atomic locking. Because untrusted clients drive this loop, also guard against attacker-triggerable stalls and resource exhaustion: keep work bounded, never let one transaction block the scheduler indefinitely, and never let lock release/queueing leave the scheduler deadlocked.

Required behavior:

1. Receive a processable transaction.
2. Select an available executor.
3. Acquire all account locks atomically.
4. If any lock conflicts, release partial locks and queue the transaction behind the blocking executor.
5. If locks succeed, execute on the assigned executor.
6. On completion, commit account changes, write ledger/status data, emit events, and mark the executor ready.

Account locks are bitmask-based:

- One `u64` per account.
- MSB represents write lock.
- Low bits represent reader executor IDs.
- This implies a hard cap of 63 executor IDs.

### SVM access validation

The forked SVM includes MagicBlock-specific access validation after execution:

> Writable accounts must be delegated, ephemeral, or confined, except for explicitly allowed cases such as fee payers, Magic Program instruction allowlists, and special post-delegation action executor patterns.

Any change touching account flags, account loading, SVM commit/rollback, or transaction sanitization must preserve this invariant. This is a security boundary: weakening it would let ordinary (untrusted) ER transactions mutate state they must not touch, which can lose funds. Do not relax it for performance or convenience.

#### Privileged accounts

Accounts carry a `privileged` flag (defined in the forked `solana-account`, accessed via `privileged()` / `set_privileged()`). The validator marks exactly one account privileged: the **validator identity (authority) account**, set in `init_validator_identity` (`magicblock-api/src/fund_account.rs`), called once during startup in `magic_validator.rs`. No other account is ever flagged privileged in this repo.

In the executor's commit-to-local-state path (`magicblock-processor/src/executor/processing.rs`) the flag grants two bypasses:

- **Persistence bypass** — privileged accounts are always written back to AccountsDb, even when not dirty (normal accounts persist only if dirty).
- **Integrity-check bypass** — when the fee payer is privileged, `verify_account_states` returns early, skipping the confined-account integrity checks that otherwise apply.

This is why the validator identity must remain privileged: validator-internal/system transactions (e.g. funding, identity operations) bypass the access-validation checks that constrain untrusted ER transactions. Do not flag additional accounts privileged, and do not remove the validator identity's privilege. (Distinct from the "privileged instruction" concept in `programs/magicblock/src/schedule_task/mod.rs`, which is about instructions disallowed inside cranks, not the account flag.)

### Sysvars

The validator supports a subset of Solana sysvars. Current documented support includes:

- `clock`
- `epoch_schedule`
- `fees` with currently imperfect fee values
- `recent_blockhashes` with currently imperfect fee values
- `rent`
- `last_restart_slot` set to `0` when enabled by feature set

Other sysvars may be stubbed or unsupported depending on whether they make sense for the ER runtime.

## Commit specification

### What commit means

Commit synchronizes account state from the ER back to the base layer while leaving the account delegated. After finalization, the PDA remains locked on the base layer under the Delegation Program.

A commit is scheduled from ER execution, not performed as a direct synchronous base-layer write by the user instruction.

### User/program entrypoint

Programs schedule commits by invoking the Magic Program from ER instructions. Current SDK examples use `MagicIntentBundleBuilder`:

- `.commit(&[account_infos])` to commit accounts.
- `.commit_and_undelegate(&[account_infos])` to commit and undelegate.
- `.add_post_commit_actions([...])` to attach Magic Actions.
- `.build_and_invoke()` to invoke the Magic Program.

For Anchor accounts, programs must ensure modified account state is serialized before the commit CPI sees it; examples call `counter.exit(&crate::ID)?` before scheduling commit after mutation.

### Magic Program scheduling instructions

The Magic Program API defines these relevant scheduling instructions:

- `ScheduleCommit`
- `ScheduleCommitAndUndelegate`
- `ScheduleBaseIntent(MagicBaseIntentArgs)`
- `ScheduleIntentBundle(MagicIntentBundleArgs)`
- `AcceptScheduleCommits`
- `ScheduledCommitSent((intent_id, bump))`

The older single-purpose `ScheduleCommit` and `ScheduleCommitAndUndelegate` paths are described as two-stage scheduling:

1. User/program instruction stages the intent in MagicContext.
2. A validator-signed `AcceptScheduleCommits` moves staged commits into the global scheduled commits map at the start of a slot so the validator can realize them immediately after.

`ScheduleIntentBundle` is the recommended bundled path for multiple independent intents with shared account overhead. It stores a scheduled intent bundle in MagicContext and logs the precomputed `ScheduledCommitSent` signature.

### MagicContext and intent IDs

When scheduling an intent bundle, the Magic Program:

1. Verifies the MagicContext account.
2. Verifies the payer signer.
3. Deserializes MagicContext.
4. Allocates the next intent ID.
5. Constructs the requested commit/action/undelegation bundle.
6. Applies cross-intent validation.
7. Charges delegated payer/fee vault or checks commit limits.
8. Adds the scheduled action to MagicContext.
9. Writes MagicContext back to account data.

### Intent validation

Bundle validation must reject invalid bundles, including:

- Empty intent bundles.
- Duplicate committed account pubkeys across the whole bundle.
- Cross references where the same account is both committed and commit-and-undelegated in incompatible fields.

### Commit fees and limits

The Magic Program code documents:

- `ACTUAL_COMMIT_LIMIT = 25` for commits covered by user DLP PDAs.
- `COMMIT_FEE_LAMPORTS = 100_000` fixed fee per commit, matching Delegation Program constants.
- Base actions have compute-unit price handling.

Changes to commit construction or scheduling should account for fee charging, fee vault behavior, and commit limits.

## Undelegation specification

### What undelegation means

Undelegation commits the latest ER state and returns account ownership from the Delegation Program to the original program on the base layer.

Product docs describe the flow as:

- **ER**: schedule commit for account(s); mark accounts as undelegating/owned by Delegation Program locally.
- **Base layer**: CPI callback gets/finalizes/recreates or updates the account from ER state and restores original program ownership.

### Local immutability after scheduling

When `ScheduleIntentBundle` includes undelegation, the Magic Program marks each undelegated account locally via `mark_account_as_undelegated`. Code comments state:

> Once account is undelegated we need to make it immutable in our validator.

This is a critical lifecycle invariant. Do not allow normal ER transactions to keep mutating an account after commit-and-undelegate has been scheduled.

Owner-program requested undelegation is recorded by the Delegation Program in
`DelegationMetadata.undelegation_requester = OwnerProgram` and an
`UndelegationRequest` PDA. This marker is not a validator-side completion
signal: the validator must still schedule commit-and-undelegate so the latest
ER state is committed/finalized before ownership returns to the owner program.

The current Delegation Program does not undelegate from finalize instructions.
Commit/finalize-style instructions only commit/finalize state and record or
preserve `DelegationMetadata.undelegation_requester`. The validator-side
committor completes undelegation by sending a standalone DLP `Undelegate`
instruction in the finalize stage. When metadata says the requester is
`OwnerProgram`, the committor must also include the undelegation request PDA in
`Undelegate`; the delegation metadata rent payer is used for both delegation
and request account cleanup. When metadata says `Validator` or is still `None`
at task-build time, no request account is passed. `None` can be valid for
validator-requested undelegation because commit and finalize task lists are
built before the commit-stage transaction records the validator requester on
base.

### Callback discriminator

The public docs specify an undelegation callback discriminator:

```text
[196, 28, 41, 206, 48, 37, 51, 167]
```

Programs must include the corresponding instruction processor, or use MagicBlock SDK macros that inject it. The callback is triggered by the Delegation Program on the base layer to revert account ownership after ER undelegation.

## Committor service specification

### Responsibility

The committor service realizes scheduled base-layer intents. Its inputs are scheduled commits/intents; its outputs are Solana base-layer transactions and confirmation results.

### Pipeline

The documented commit pipeline is:

1. Magic Program schedules intents in MagicContext.
2. `magicblock-accounts` / scheduled commit processing picks up intents each slot.
3. `magicblock-committor-service` schedules and executes intents.
4. Task building creates atomic base-layer tasks such as commit, undelegate, finalize, and action.
5. Task strategy packs tasks into valid transactions.
6. Delivery preparation handles address lookup tables and commit buffers.
7. RPC client sends and confirms transactions.
8. SQLite persister preserves state across restarts.

### Task strategy

Commit tasks may be represented in different forms:

- `ArgsTask`: commit data passed as instruction args.
- `BufferTask`: commit data uploaded through buffers, used for large changesets.

The strategist may convert args tasks into buffer tasks when needed to fit transaction constraints.

### Parallelism and blocking

The committor service uses scheduling because intents can block one another. It can run multiple intent executors in parallel, but scheduling must respect conflicts/dependencies between intents. Preserve this parallelism and avoid avoidable throughput regressions in commit preparation, task packing, buffer upload, ALT handling, send/confirm loops, and persistence; call out any unavoidable performance tradeoff explicitly.

## Magic Actions specification

Magic Actions attach base-layer call instructions that run automatically after an ER commit. They allow committed state to drive base-layer workflows.

Properties:

- Actions are scheduled with a commit or as standalone base actions.
- Actions specify destination program, short account metas, args, escrow authority, and compute units.
- Secure action handling should identify the source program where applicable.
- Actions run on the base layer after the relevant committed state is available.

## Ephemeral account specification

Ephemeral accounts are ER-only accounts.

Properties from public docs and Magic Program API:

- Created, resized, and closed in the ER through Magic Program instructions.
- Owned by the calling program.
- Funded by a sponsor account that must be delegated.
- Rent is currently specified as `32 lamports/byte` with `60` bytes overhead.
- Sponsor pays additional rent on grow and receives refunds on shrink/close.
- Ephemeral account signer is required on create to prevent pubkey squatting, but not for resize/close.

Magic Program instructions:

- `CreateEphemeralAccount { data_len }`
- `ResizeEphemeralAccount { new_data_len }`
- `CloseEphemeralAccount`

## RPC and router specification

The MagicBlock Router API implements most standard Solana JSON-RPC methods and adds MagicBlock-specific methods.

Important ER/router methods include:

- `getDelegationStatus`
- `getBlockhashForAccounts`
- `getRoutes`
- router-aware `getAccountInfo` and signature status methods

The validator RPC layer should remain aligned with Solana JSON-RPC expectations where it implements standard methods, while supporting MagicBlock-specific delegation and local-clone behavior. RPC changes must preserve low-latency request handling and avoid unnecessary blocking, clone/fetch amplification, or heavy per-request work on hot paths.

## Startup, shutdown, and recovery specification

### Startup

The documented validator startup sequence is:

1. Open ledger.
2. Sync keypair.
3. Open AccountsDb.
4. Connect replication broker and optionally fetch snapshot.
5. Initialize committor service.
6. Initialize chainlink.
7. Initialize genesis accounts.
8. Initialize metrics.
9. Load programs.
10. Spawn transaction scheduler in `StartingUp` mode.
11. Spawn RPC thread.
12. Initialize task scheduler.
13. On `start()`: optionally replay ledger, defragment AccountsDb, reset stale accounts, recover pending commit intents.
14. Switch scheduler to `Primary` or `Replica` mode.

### Shutdown

Shutdown ordering matters:

1. Trigger cancellation tokens.
2. Stop services while preserving in-flight intent safety.
3. Stop committor service last among services where needed for in-flight intents.
4. Join threads.
5. Flush AccountsDb and ledger.
