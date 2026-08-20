# Chainlink Account Materialization Semantics

Authoritative reference for how Chainlink coordinates fetch and
subscription-update materialization so the freshest remote account state is
retained and post-delegation actions execute exactly once per delegation.
Answers the questions raised in
[#1541](https://github.com/magicblock-labs/magicblock-validator/issues/1541).

## The race

`RemoteAccountProvider::try_get_multi` subscribes before fetching. When
`listen_for_account_updates` receives a subscription update whose slot is
`>= fetch_start_slot` while a fetch is pending, it does both of the following:

1. Resolves the pending fetch waiters with the subscription data (the RPC
   result is discarded when it arrives).
2. Forwards the same update to `FetchCloner::process_subscription_update`,
   because fetch waiters may not clone the result (e.g. status reads) and
   SubMux dedup already dropped every other copy of the update.

The fetch and subscription paths can therefore race to materialize the same
account. This is by design; the rules below make the race safe.

## Single materialization model

There is no create/update split. Every input — fetch result, subscription
update, greedy DLP discovery — is reduced to the same request shape and
funneled through one path:

```
AccountCloneRequest { account, commit_frequency_ms, post_delegation_mode, delegated_to_other }
  └── clone_account_with_post_delegation_action_invariants
        └── clone_account_with_ownership
              └── Cloner::clone_account
```

`ClonePostDelegationMode` is mutually exclusive (#1504):

- `None` — plain clone.
- `ExecuteActions(DelegationActions)` — clone, then execute post-delegation
  actions after activation.
- `RescueUndelegate` — clone, then schedule undelegation because the actions
  cannot be executed safely.

Every materialization writes a full account image. "Create" is simply a
materialization when the bank holds nothing yet; both paths can perform it.

### Ownership of concurrent work

Two independent ownership layers serialize the race:

1. **Fetch arbitration** (`RemoteAccountProvider::fetching_accounts`): a
   per-pubkey entry with a monotonic generation decides who *produces* the
   freshest remote image. Concurrent `try_get_multi` callers join as waiters;
   a fresh-enough subscription update steals the entry and resolves all
   waiters. Stale updates (slot `< fetch_start_slot`) put the entry back and
   are dropped.
2. **Clone claim** (`FetchCloner::claim_pending_clone`): a per-pubkey
   owner/waiter protocol decides who *materializes* the image. Exactly one
   owner runs `Cloner::clone_account`; waiters block on completion, then
   re-check `local_account_satisfies_clone_request` and skip when the bank
   already satisfies their slot.

## Ordering semantics

Three layered rules, evaluated in order:

1. **Remote slot monotonicity.** A materialization never overwrites bank state
   with a lower `remote_slot`. Same-slot updates are non-advancing and dropped,
   with one exception: the bank holds a plain or undelegating image while the
   update carries the *delegated* state at the same slot (the
   undelegate→redelegate same-slot refresh).
2. **Local mode authority.** While the bank image is delegated and not
   undelegating, the ephemeral validator is authoritative: remote updates are
   dropped entirely and the direct subscription is released. While
   undelegating, remote updates apply only when they prove undelegation
   completion or redelegation (rule 3).
3. **Delegation generation.** The generation ordinal is the on-chain
   `delegation_record.delegation_slot` — no additional per-account state is
   kept. For an undelegating bank image receiving a delegated chain image
   (`account_still_undelegating_on_chain`):

   | Chain state | Condition | Verdict |
   |---|---|---|
   | Delegated to us | `delegation_slot <= bank remote_slot` | Echo of our own commit — undelegation still pending, keep bank image |
   | Delegated to us | `delegation_slot > bank remote_slot` | Redelegation (new generation) — clone, clear undelegating |
   | Delegated elsewhere | record present | Undelegation completed — clone as-is |
   | Not delegated | no record | Undelegation completed — clone as-is |

## Post-delegation actions: exactly-once

Actions are not bound to a particular update or path; they are bound to the
**transition into locally-delegated mode**:

- Every path that materializes a delegated account resolves the delegation
  record first (fetch classification, subscription resolution, greedy
  discovery), and the record carries the actions. Whichever path wins the race
  therefore carries the current generation's actions.
- Once the bank image is delegated, `local_delegated_clone_target_active`
  short-circuits every later delegated clone request, so a losing racer can
  never replay actions.
- A redelegation after transient undelegation is a new generation (rule 3
  above): the update clones with the *new* record's actions, which execute
  once on that transition.

### Atomic failure: rescue undelegation

If action dependencies cannot be fetched or the actions contain unsafe
signers, activation fails atomically: the account is still cloned (so state is
not lost) but with `RescueUndelegate`, which schedules automatic undelegation
back to chain. Actions from a failed activation are never partially executed.

## Ownership boundary

- **RemoteAccountProvider** owns fetch/subscription arbitration and slot
  classification: which remote image is freshest, and who delivers it.
- **FetchCloner** owns materialization semantics: delegation-record
  resolution, ordering rules, local account mode, and action invariants.
- **Cloner / Engine** executes the clone transaction and the post-delegation
  actions inside the bank.

No component performs another's role; removing this boundary (or adding a
parallel materialization path) reintroduces the split-brain risk this model
eliminates.

## Regression scenarios

The interleavings that must stay correct:

1. Subscription update resolves a pending fetch and is also forwarded — the
   account materializes once, with actions, whichever consumer runs first.
2. Fetch result arrives after a fresher subscription update — the RPC result
   loses arbitration and is discarded.
3. Subscription update older than the bank image — dropped by monotonicity.
4. Delegation with actions discovered concurrently by fetch and subscription —
   one clone claim wins, the loser skips, actions run once.
5. Same-slot undelegate→redelegate — the delegated refresh exception admits
   the update.
6. Transient undelegation followed by redelegation with new actions — the
   `delegation_slot` comparison classifies it as a new generation and the new
   actions execute once.
7. Unsatisfiable actions — the account clones with `RescueUndelegate` and is
   automatically undelegated.

## Critical-path impact

- **Locking:** the subscription fast path forwards without the transition lock
  or per-key guard; per-key guards apply only while a fetch is pending or the
  account sits in the secondary tier. Clone claims are per-pubkey.
- **Allocation:** no unbounded per-account state; generations derive from the
  on-chain record, and pending-fetch/pending-clone entries are transient.
- **Remote calls:** delegation-record resolution rides the existing companion
  fetch; no duplicate account fetches are issued for the race itself.
- **Scheduler:** subscription updates process under a bounded semaphore
  (`SUBSCRIPTION_UPDATE_LIMIT`); clone waiters park on oneshot channels.

## Known limitations

1. **Same-slot redelegation ambiguity.** `delegation_slot == bank remote_slot`
   is deliberately classified as "still undelegating" (the opposite choice
   caused incorrect unborking; see the disabled subcase B1 test in
   `account_still_undelegating_on_chain.rs`). A delegate→undelegate→redelegate
   sequence inside one slot with no later update can leave the account stuck
   undelegating.
2. **Restart durability.** "Actions already executed" lives only in the bank's
   in-memory delegated mode; a restart in the activation window can replay or
   drop actions for the in-flight generation.
