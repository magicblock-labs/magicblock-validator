# `magicblock-chainlink`

## Purpose

`magicblock-chainlink` is the validator's base-chain account synchronization crate. It is the bridge between Solana RPC/pubsub state and the validator's local `AccountsDb`.

At a high level it:

- fetches accounts from the base layer when RPC reads or transaction submission need them locally,
- subscribes to base-layer account/program updates and turns those updates into local clone operations,
- resolves delegation records for DLP-owned accounts and rewrites local account metadata so delegated accounts execute under their original owners,
- keeps local copies fresh while avoiding duplicate concurrent fetches/clones,
- handles program-account loading, associated-token/eATA projection, post-delegation action dependencies, and undelegation tracking,
- owns subscription capacity/LRU bookkeeping and defensive eviction signaling.

This crate prepares local state for execution. It does **not** decide final post-execution write validity; the processor/SVM path still enforces MagicBlock writable-account invariants.

Chainlink is on the account-availability hot path for RPC reads and transaction submission. Changes must preserve low-latency fetch/clone behavior, bounded subscription overhead, deduplication, and low contention. Do not introduce avoidable duplicate remote fetches/clones, subscription churn, blocking work, excessive logging, or heavy per-account allocations/serialization; call out any unavoidable performance tradeoff explicitly.

## Update requirement

Whenever behavior in `magicblock-chainlink` changes, or another crate changes Chainlink flows, update this document in the same change for changes to:

- account fetch/clone classification,
- delegation-record resolution or local delegated/confined/undelegating flags,
- subscription ownership, LRU eviction, reconnection, or update ordering,
- program loading,
- ATA/eATA projection,
- post-delegation action dependency handling,
- lifecycle-mode behavior,
- public APIs used by `magicblock-api`, `magicblock-aperture`, `magicblock-accounts`, `magicblock-account-cloner`, or `programs/magicblock`,
- tests or validation commands relevant to this crate,
- performance characteristics of fetch/clone, deduplication, subscription, LRU/eviction, or update-ordering paths.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

Primary source files:

| Path | Role |
|---|---|
| `magicblock-chainlink/src/lib.rs` | Crate exports. Re-exports Chainlink types and `AccountFetchOrigin`. |
| `magicblock-chainlink/src/chainlink/mod.rs` | Public Chainlink facade, replication-mode wrapper, transaction/account ensure entrypoints, removed-account eviction listener. |
| `magicblock-chainlink/src/chainlink/fetch_cloner/` | Main fetch/clone pipeline, delegation handling, subscription-update processing, ATA/eATA projection, pending operation deduplication. |
| `magicblock-chainlink/src/remote_account_provider/` | RPC/pubsub provider, subscription ownership, LRU capacity, websocket/gRPC clients, program-account resolution. |
| `magicblock-chainlink/src/submux/` | Multiplexes multiple pubsub clients, deduplicates/debounces updates, reconnects clients, fans updates into one stream. |
| `magicblock-chainlink/src/cloner/mod.rs` | `Cloner` trait implemented by `magicblock-account-cloner`; request types passed from Chainlink to the clone executor. |
| `magicblock-chainlink/src/accounts_bank.rs` | Test/mock-oriented `AccountsBank` helpers for this crate. |
| `magicblock-chainlink/src/testing/` | Test support behind `dev-context`. |
| `magicblock-chainlink/tests/` | Integration-style Chainlink tests for account ensure, delegation, redelegation, ordering, and race recovery. |

Main consumers:

- `magicblock-api` constructs the production Chainlink stack during validator startup.
- `magicblock-aperture` uses Chainlink for RPC read misses and transaction submission account availability.
- `magicblock-accounts` uses Chainlink/account cloning glue for account-manager flows and scheduled commit integration.
- `magicblock-account-cloner` implements the `Cloner` trait and submits clone/program/evict transactions into the local validator.
- `programs/magicblock` uses `dev-context` Chainlink helpers in tests and validator-only program flows.

## Main public types and APIs

### Chainlink facade

`src/chainlink/mod.rs` defines the main stack:

- `InnerChainlink<T, U, V, C>`: active Chainlink implementation parameterized by RPC client, pubsub client, accounts bank, and cloner.
- `ReplicationModeAwareChainlink<T, U, V, C>`: wrapper with `Enabled` and `Disabled` modes.
- `ProdInnerChainlink<C>` / `ProdChainlink<C>`: production aliases using `ChainRpcClientImpl`, `SubMuxClient<ChainUpdatesClient>`, `AccountsDb`, and a configurable cloner.

Important methods:

- `try_new_from_endpoints(...)`: builds `RemoteAccountProvider`, `FetchCloner`, risk service, and subscription update channel from configured base-layer endpoints.
- `ensure_transaction_accounts(tx)`: ensures all transaction account keys, plus a possible fee-payer ephemeral balance PDA, are present locally. No-op system transfers are skipped.
- `ensure_accounts(pubkeys, mark_empty_if_not_found, fetch_origin)`: fetches/clones accounts but returns only fetch/clone status.
- `fetch_accounts(pubkeys, fetch_origin)`: ensures accounts and then reads them from the local bank.
- `accounts_delegated_on_base_and_er(pubkeys, fetch_origin)`: checks that each account is DLP-owned on base and represented as delegated/DLP-owned locally.
- `undelegation_requested(pubkey)`: called by committor/account flows before an account is undelegated so Chainlink keeps watching for base-layer completion.
- `fetch_count()` / `is_watching()`: mainly observability/testing helpers.

Disabled replication mode is intentionally conservative:

- `ensure_accounts` is a no-op success.
- `fetch_accounts` returns `None` for each requested account.
- `ensure_transaction_accounts` errors with `DisabledForNonPrimaryMode`.
- undelegation tracking is ignored.

### `Cloner` interface

`src/cloner/mod.rs` defines the boundary between Chainlink and local clone execution:

- `AccountCloneRequest` carries `pubkey`, resolved `AccountSharedData`, optional `commit_frequency_ms`, post-delegation `DelegationActions`, and optional `delegated_to_other` authority.
- `DelegationActions` wraps post-delegation action instructions from delegation records.
- `Cloner` trait methods:
  - `clone_account(request)`,
  - `clone_program(LoadedProgram)`,
  - `evict_account(pubkey)`.

Chainlink should build accurate clone requests; the cloner owns how those requests are materialized in the local validator.

## Runtime flow: transaction account ensure

`ensure_transaction_accounts` performs the normal transaction-preparation flow:

1. Skip no-op system transfer transactions (`filters/noop_system_transfer.rs`).
2. Collect all account keys from the sanitized transaction.
3. Derive `ephemeral_balance_pda_from_payer(fee_payer, 0)` and add it if absent locally.
4. Mark all collected pubkeys as `mark_empty_if_not_found`; missing transaction accounts are cloned as empty placeholders when appropriate.
5. Call `ensure_accounts` with `AccountFetchOrigin::SendTransaction(signature)`.
6. `ensure_accounts` promotes accounts in the subscription LRU and calls `FetchCloner::fetch_and_clone_accounts_with_dedup`.

Pitfalls:

- This method only ensures availability. It must not loosen execution access rules.
- `mark_empty_if_not_found` is broad for transaction submission by design; changing it can affect how missing fee-payer/escrow/transaction accounts appear to execution.
- The fee-payer balance PDA logic must stay aligned with Magic Program ephemeral balance handling.

## Runtime flow: fetch and clone pipeline

The central implementation is `FetchCloner::fetch_and_clone_accounts_with_dedup` and its inner `fetch_and_clone_accounts`.

### Deduplication and bank fast path

Before fetching remotely:

1. Blacklisted accounts are filtered out.
2. Existing non-undelegating accounts in `AccountsDb` are treated as ready.
3. Existing undelegating accounts are checked asynchronously by `should_refresh_undelegating_in_bank_account` to see whether base-layer undelegation completed.
4. Remaining pubkeys enter `pending_requests` ownership coordination.

Only the first caller for a pubkey owns the fetch/clone operation. Later callers become waiters and receive the owner's result. Preserve this behavior for both correctness and performance; regressions here can amplify RPC traffic, clone transactions, and transaction-submission latency. Pending owners have:

- generation IDs to avoid stale cleanup,
- cancellation hooks,
- a default timeout of `FETCH_CLONE_OPERATION_TIMEOUT` (60 seconds),
- waiter-specific result filtering so each caller sees only the entries for its pubkey.

There is a second dedup layer for actual clone transactions: `pending_clones` is keyed by `(pubkey, remote_slot)`, so concurrent fetch and subscription paths do not submit duplicate local clone operations for the same account version.

### Remote fetch

`RemoteAccountProvider::try_get_multi` subscribes before fetching so subscription updates that arrive during the fetch can win over stale RPC data. It:

1. Claims entries in `fetching_accounts` for pubkeys not already being fetched.
2. Sets up direct account subscriptions for claimed pubkeys.
3. Starts an RPC fetch with `min_context_slot` equal to the observed chain slot or requested slot.
4. Waits for either RPC results or a subscription update that is at least as new as the fetch start slot.
5. Returns results in input order.

RPC fetches use Base64Zstd encoding, commitment from the RPC client, `min_context_slot`, timeout/retry handling, and metrics for success/found/not-found/failure.

### Classification

`pipeline::classify_remote_accounts` divides fetched accounts into:

- `not_found`: missing on chain,
- `plain`: normal non-executable accounts not owned by DLP,
- `owned_by_deleg`: accounts currently owned by the Delegation Program,
- `programs`: executable accounts,
- `atas`: associated token accounts recognized by supported token-program layouts.

`partition_not_found` further separates missing accounts into:

- `clone_as_empty`: requested via `mark_empty_if_not_found`,
- `not_found`: left absent so later code fails naturally if it needs them.

### Delegated account resolution

DLP-owned accounts must be resolved with their delegation record before cloning:

1. Derive `delegation_record_pda_from_delegated_account(account_pubkey)`.
2. Acquire a `DelegationRecord` subscription reason for the record PDA.
3. Fetch account and delegation record with slot matching via `try_get_multi_until_slots_match`.
4. Parse `DelegationRecord` and optional post-delegation actions.
5. Apply local metadata:
   - owner is set to `delegation_record.owner`,
   - `confined` is set when `authority == Pubkey::default()`,
   - `delegated` is set when authority is this validator or confined, except raw eATA PDAs are not marked delegated directly,
   - `commit_frequency_ms` is included only for accounts delegated/confined to this validator.
6. If authority belongs to another validator, `delegated_to_other` is set on the clone request.
7. Missing non-internal delegation records are reported in `FetchAndCloneResult::missing_delegation_record`.

Important caveats:

- Invalid delegation records are fatal for the fetch/clone operation because local ownership would be ambiguous.
- Post-delegation actions are parsed/decrypted only when the record authority is this validator.
- Confined accounts (`authority == Pubkey::default()`) are treated as locally delegated for execution purposes but also marked confined.
- DLP-internal accounts may be cloned without a delegation record if `is_internal_dlp_account_data` recognizes the layout.
- Delegated direct account subscriptions are cleaned up after delegation is discovered; delegated state is locally authoritative until undelegation tracking is requested.

### Post-delegation actions

Delegation records may carry encrypted or cleartext post-delegation actions. Chainlink:

- parses actions from data after `DelegationRecord::size_with_discriminator()`,
- decrypts them with the validator keypair when needed,
- validates signer addresses through `RiskService` when configured,
- collects action dependencies from instruction program IDs and account metas,
- force-refreshes writable dependencies that are absent or not currently delegated,
- errors with `MissingDelegationActionAccounts` if required delegated writable dependencies cannot be resolved.

Do not execute or ignore these actions blindly. They are part of clone-time invariants for post-delegation behavior.

### Program account resolution

Executable accounts are converted into `LoadedProgram` values and passed to `Cloner::clone_program`.

Supported loader handling lives in `remote_account_provider/program_account.rs`:

- Loader V1: deprecated; subscription updates for V1 are unexpected.
- Loader V2: single account contains metadata/data.
- Loader V3: program account plus separate program-data account; Chainlink fetches both with matching slots and holds a `ProgramData` subscription reason while resolving.
- Loader V4: single account with loader-v4 state and deployable data handling.

Program clone restrictions:

- `allowed_programs` from config, when non-empty, limits program cloning.
- native loader accounts should be blacklisted and are not cloned.
- LoaderV3 program-data subscriptions must be released on success and error paths.

### ATA/eATA projection

Chainlink has special handling for associated token accounts and ephemeral ATAs:

- Base ATAs are recognized via `magicblock_core::token_programs::is_ata`.
- For each ATA, Chainlink derives the companion eATA PDA with `try_derive_eata_address_and_bump`.
- It subscribes to both ATA and eATA using `SubscriptionReason::AtaProjection`.
- If the eATA exists, has a delegation record for this validator, and can be projected, Chainlink clones a projected delegated ATA into the local bank.
- Projection preserves the base ATA's owner and data length, which is important for Token-2022 extensions.
- Missing eATAs can be remembered in `known_empty_eatas`, but only after confirmed `NotFound` while an eATA subscription is live.
- Raw eATA PDAs are not marked delegated directly; their state is projected into the corresponding base ATA.

Pitfalls:

- Do not rebuild Token-2022 accounts as legacy SPL Token accounts; use the projection helpers that preserve layout.
- Same-slot delegated refreshes are a narrow ordering exception for allowing a delegated update over plain/undelegating local state at the same `remote_slot`. They do not mean same-slot re-delegation to the same validator is fully supported; without a delegation generation/index, `account_still_undelegating_on_chain` cannot distinguish `delegation_slot == remote_slot_in_bank` from a still-pending undelegation, and `magicblock-chainlink/tests/07_redeleg_us_same_slot.rs` remains ignored for that reason.
- Undelegating ATAs may remain in bank while a companion eATA is still delegated to this validator.

## Runtime flow: subscription updates

Base-layer subscription updates flow through:

```text
ChainUpdatesClient / ChainPubsubClientImpl / ChainLaserClientImpl
  -> SubMuxClient
  -> RemoteAccountProvider::listen_for_account_updates
  -> FetchCloner::start_subscription_listener
  -> FetchCloner::process_subscription_update
  -> Cloner::clone_account / clone_program
```

Key behavior:

- Clock sysvar updates update `chain_slot` and are not forwarded to the fetch cloner.
- Non-clock updates become `ForwardedSubscriptionUpdate` with a `SubscriptionSource` (`Account` or program source).
- If a subscription update arrives while an RPC fetch is pending and its slot is at least the fetch start slot, it resolves the pending fetch waiters instead of being forwarded as a separate update.
- Account-subscription updates for pubkeys no longer watched are dropped and can enqueue a removal update if stale local state exists.
- Program-subscription updates are allowed even if the pubkey is not in the direct-account LRU; delegated accounts may be tracked only by owner-program subscriptions.
- Non-advancing updates are ignored unless they represent a same-slot delegated refresh needed for undelegate/redelegate recovery.
- Delegated updates cause direct subscription cleanup; undelegation-completion updates retain/directly ensure subscriptions as appropriate and release `UndelegationTracking` ownership.

### Greedy discovery

If a subscription update discovers a DLP-owned account absent from the bank, Chainlink may greedily fetch and clone it if the delegation record says it belongs to this validator (or is confined). This is especially important for delegated eATA discovery and owner-program subscriptions.

Updates delegated to other validators are ignored after discovery so this validator does not clone state it cannot execute against.

## RemoteAccountProvider internals

`RemoteAccountProvider` owns direct remote access and subscription state.

### Endpoints

Endpoint setup requires at least one RPC endpoint and at least one usable pubsub endpoint when lifecycle mode needs remote sync.

Supported pubsub endpoint variants:

- WebSocket via `ChainPubsubClientImpl`,
- gRPC/Laserstream via `ChainLaserClientImpl`,
- RPC endpoints are used for fetches, not pubsub.

Startup chooses gRPC clients first when any gRPC endpoint exists because they can backfill subscriptions cheaply. WebSocket clients may be attached later as deferred clients. If gRPC startup fails and WebSocket fallback exists, startup retries with WebSocket.

### Chain slot

`chain_slot` is monotonic and updated from:

- clock account websocket updates,
- gRPC slot updates.

Fetches use `min_context_slot` to avoid serving account data older than the freshest observed slot or required companion slot.

### Subscription ownership reasons

A pubkey can be held for multiple reasons:

- `DirectAccount`: normal account monitoring and normal LRU capacity management.
- `DelegationRecord`: temporary/explicit monitoring for delegation record PDAs.
- `ProgramData`: LoaderV3 program-data accounts.
- `UndelegationTracking`: protected monitoring while an account is expected to complete undelegation on base.
- `AtaProjection`: ATA/eATA projection monitoring.

Ownership is reference-counted per reason. Releasing one reason does not unsubscribe while other reasons remain.

`ensure_subscription` differs from `acquire_subscription`: it does not increment an already-held reason. This is used by eATA projection to keep an LRU entry warm without unbounded refcount growth.

### LRU and defensive eviction

`AccountsLruCache` bounds monitored direct-account subscriptions. On capacity pressure:

- never-evicted accounts are skipped,
- accounts currently delegated or undelegating in the bank are protected,
- accounts with `UndelegationTracking` ownership are protected,
- if no candidate can be evicted, the new subscription is unsubscribed and `NoEvictableSubscriptionCapacity` is returned.

When an account is evicted from subscription capacity, the provider sends a removal update. `InnerChainlink::subscribe_account_removals` listens for these and may submit `Cloner::evict_account` to remove stale local state, but only if the bank account is neither delegated nor undelegating.

Removal handling is serialized with same-pubkey subscription transitions via `evict_unwatched_with_subscription_lock`, preventing an evict transaction from being submitted after a fresh subscription re-watches the same pubkey.

### Reconciliation

If subscription metrics are enabled, a background task periodically runs `subscription_reconciler::reconcile_subscriptions` to compare the LRU with actual pubsub-client subscriptions, update metrics, and notify removal for subscriptions that vanished.

## SubMuxClient internals

`SubMuxClient<T>` wraps multiple pubsub clients and implements `ChainPubsubClient`.

Responsibilities:

- fan out account subscribe/unsubscribe requests to inner clients,
- fan out program subscriptions,
- fan in updates into one receiver,
- suppress duplicate `(pubkey, slot)` updates across clients within a dedupe window,
- debounce high-frequency account streams by forwarding at most the latest update per interval,
- never debounce the clock sysvar,
- reconnect clients after abort signals and resubscribe all tracked accounts/program subscriptions,
- expose subscription union/intersection and connection metrics.

Default timing constants:

- output channel size: `5_000`,
- dedupe window: `2_000ms`,
- debounce interval: `2_000ms`,
- debounce detection window: 5x the selected interval by default.

Changing SubMux behavior can affect ordering, duplicate clone submissions, and perceived account freshness. Use the ordering and redelegation tests when changing it.

## Lifecycle mode and configuration

`ChainlinkConfig` wraps `RemoteAccountProviderConfig` and currently includes `remove_confined_accounts`.

`RemoteAccountProviderConfig` includes:

- subscription LRU capacity (`DEFAULT_MAX_MONITORED_ACCOUNTS` by default),
- validator lifecycle mode,
- subscription metrics flag,
- startup program subscriptions (defaults to the Delegation Program),
- resubscription delay (`DEFAULT_RESUBSCRIPTION_DELAY_MS` by default),
- global gRPC config.

The remote provider is constructed only when `lifecycle_mode().needs_remote_account_provider()` is true. Offline/disabled modes must keep bank-only/no-op behavior intact.

## Important invariants

This crate is security-critical: it is the validator's only source of truth about base-layer (Solana) account state, and that truth ultimately governs which funds can move and settle. Keeping local state in sync with the base layer is a security requirement, not just a correctness/performance one (see `.agents/rules/validator-goals.md` and `.agents/specs/validator-specification.md`). Under no circumstances may a change make synchronization weaker, less stable, or more permissive than it is today:

- Subscriptions (websocket/gRPC), fetching, delegation-record resolution, slot/`min_context_slot`/commitment handling, and clone-freshness checks must stay at least as strong and stable as now.
- The validator must never serve or execute against stale, forged, or out-of-sync state, never mark an account delegated without the authority checks below, and never miss base-layer updates that change delegation/undelegation truth.
- Because subscription/fetch updates are driven by external base-layer events and untrusted submissions, treat the dedup, slot-matching, ordering, LRU-protection, and bounded-capacity logic as security controls against races, stale-overwrite, and resource exhaustion. Do not relax them for performance.

Preserve these invariants when editing this crate:

1. **Never clone DLP-owned state as writable delegated state without a valid delegation record**, except explicitly recognized internal DLP accounts.
2. **Delegated local accounts must be presented with their original owner**, not the Delegation Program owner.
3. **Authority matters**: this validator can mark accounts delegated only when the record authority is this validator or the confined/default authority.
4. **Delegated and undelegating local accounts are protected from subscription-capacity eviction and defensive bank eviction.**
5. **Subscription update ordering must not overwrite fresher local state with older or duplicate data.** Same-slot delegated refresh is a narrow redelegation recovery exception.
6. **Fetches that need companion accounts must use matching slots or a minimum context slot** so account and delegation/program-data records are coherent.
7. **Pending request and pending clone deduplication must clean up by generation/key** to avoid stale owners unblocking or deleting newer work.
8. **Program-data subscriptions for LoaderV3 must be cleaned up on all paths.**
9. **ATA/eATA projection must preserve base ATA layout and token-program ownership.**
10. **Post-delegation action dependencies must be available before clone-time action handling.**
11. **Disabled/non-primary mode must not perform remote fetches or transaction account ensures.**
12. **This crate must not weaken processor/SVM access validation.** It only prepares local account state.
13. **Fetch/clone and subscription paths must remain performance-conscious.** Preserve deduplication, bounded waiting, LRU protections, low subscription churn, and non-blocking behavior unless a documented correctness requirement forces a tradeoff.

## Common change areas and what to inspect

### Account not found, stale account, or wrong owner

Start with:

- `InnerChainlink::ensure_accounts`,
- `FetchCloner::fetch_and_clone_accounts_with_dedup`,
- `FetchCloner::fetch_and_clone_accounts`,
- `pipeline::classify_remote_accounts`,
- `pipeline::resolve_delegated_accounts`,
- `delegation::apply_delegation_record_to_account`.

Check whether the account is blacklisted, already in bank, undelegating, missing a delegation record, delegated to another validator, or projected from eATA.

### Subscription update bugs

Start with:

- `RemoteAccountProvider::listen_for_account_updates`,
- `FetchCloner::process_subscription_update`,
- `RemoteAccountProvider::{acquire_subscription, release_single_subscription, release_subscription_reason_silently_for_delegated_account}`,
- `SubMuxClient` dedupe/debounce/reconnect logic,
- `subscription_reconciler`.

Pay special attention to `SubscriptionSource::Account` vs program-source updates.

### LRU/eviction bugs

Start with:

- `RemoteAccountProvider::register_subscription`,
- `CapacityEvictionProtection`,
- `InnerChainlink::subscribe_account_removals`,
- `RemoteAccountProvider::evict_unwatched_with_subscription_lock`.

Do not evict delegated or undelegating local state.

### Redelegation or undelegation bugs

Start with:

- `FetchCloner::should_refresh_undelegating_in_bank_account`,
- `FetchCloner::process_subscription_update`,
- `account_still_undelegating_on_chain.rs`,
- `undelegation_requested`,
- tests `04` through `09`.

Same-slot cases are intentionally covered by separate tests.

### Program clone bugs

Start with:

- `pipeline::resolve_programs_with_program_data`,
- `program_loader::handle_executable_sub_update`,
- `remote_account_provider/program_account.rs`,
- `allowed_programs` config.

### ATA/eATA bugs

Start with:

- `ata_projection.rs`,
- `delegation::parse_raw_eata_pda`,
- `maybe_greedily_clone_discovered_delegated_account`,
- `process_subscription_update` projected clone path.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-chainlink`.
- Relevant integration suites: `test-chainlink`; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Useful Chainlink test files: `magicblock-chainlink/tests/basics.rs`, `01_ensure-accounts.rs`, `03_deleg_after_sub.rs`, redelegation tests `04` through `07`, `08_subupdate-ordering.rs`, and `09_waiter_reconciliation_race.rs`.
- Performance validation intent: fetch/clone, subscription, LRU, or update-ordering hot-path changes should include the smallest practical test or measurement that can expose duplicate fetches/clones, increased latency, contention, or subscription churn; if skipped, report the residual performance risk.

## Adjacent implementation references

- `.agents/context/crates/magicblock-account-cloner.md` — clone request materialization boundary implemented by the production cloner.
- `.agents/context/crates/magicblock-accounts.md` — scheduled commit integration and undelegation notification consumer.
- `.agents/context/crates/magicblock-aperture.md` — RPC read and transaction submission account-ensure caller.
- `.agents/context/crates/magicblock-aml.md` — signer risk-check integration for post-delegation actions.
- `magicblock-chainlink/src/cloner/mod.rs` — `Cloner`, `AccountCloneRequest`, and `DelegationActions` boundary.
- `magicblock-chainlink/src/chainlink/fetch_cloner/` — fetch/clone pipeline, delegation handling, ATA/eATA projection, and pending operation deduplication.
- `magicblock-chainlink/src/remote_account_provider/` — RPC/pubsub provider, subscription ownership, LRU capacity, and program-account resolution.
- `magicblock-chainlink/tests/` — Chainlink account ensure, delegation, ordering, and race-recovery tests.
