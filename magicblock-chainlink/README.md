# `magicblock-chainlink`

Synchronizes base-chain accounts and programs into Engine for the leader.
Chainlink owns remote fetching, subscriptions, delegation classification, and
materialization policy. Engine owns local account storage and execution.

## Account loading

`ensure_accounts` fills missing local accounts for application execution.
Remote absence can become a placeholder; it is not permission to invent a
delegation or writable account. Fetch/status APIs distinguish remote evidence
from local readiness.

The remote account provider arbitrates concurrent fetches and subscription
updates. Pending fetches carry generation ownership so an old completion cannot
remove a newer request. A subscription update can satisfy fetch waiters and
still be forwarded for materialization: not every fetch consumer writes local
state.

## Materialization and ordering

Fetch, subscription, and discovery paths converge on `AccountCloneRequest`.
Before applying an image, `claim_materialization` obtains an Engine account
accessor and classifies it against the current local image:

- local ephemeral state and an already-active delegated image satisfy matching
  delegation work without overwriting local execution;
- older remote images and identical images do not require a write;
- conflicting images in the same mode at the same slot are errors;
- mode changes must satisfy Engine's slot-aware transition rules;
- a missing remote image completing transient undelegation is normalized to
  read-only instead of attempting a transient-to-placeholder transition.

Classification and submission share the account accessor. Do not add a
separate clone-claim cache or a write path that bypasses this boundary.

## Activation actions and rescue

Resolve delegation evidence and prepare action dependencies before taking the
materialization claim. Actions are only valid for delegated targets and carry
their source-program provenance into Engine's post-finalization path.
[A risk check][risk] can reject activation; it cannot grant mutation authority.

Successful activation materializes the account and executes its actions in the
Engine transaction. Already-satisfied local delegation work skips reactivation.
If dependency preparation or activation fails, Chainlink attempts materialization
with a rescue undelegation action. That attempt can also fail; the error is
propagated, and scheduling rescue is not proof that base-chain undelegation has
completed.

These mechanisms are not a blanket exactly-once guarantee across provider races,
crashes, and cross-chain delivery. See [activation contracts][activation] and
[account protection][protection] for the broader obligations and evidence gaps.

## Programs and projected accounts

Program handling resolves loader-specific metadata and executable data rather
than treating every program as one ordinary account. Retain loader readiness,
program-data subscription cleanup, and invalid/retracted-program checks.

Token-account projection combines base account and companion delegation state;
shape alone is not authority. Confined zero-lamport state is distinct from
ordinary delegated or sponsored ephemeral state. Preserve those classifications
when changing fetch batching or subscription processing.

Shutdown stops Chainlink's background synchronization before the leader finishes
Engine shutdown. Remote RPC latency, companion fetches, and materialization
contention remain on the account-loading path; do not describe it as free or
universally non-blocking.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[risk]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-aml/README.md
[activation]: https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/activation-and-actions.md
[protection]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/account-lifecycle/account-protection.md
