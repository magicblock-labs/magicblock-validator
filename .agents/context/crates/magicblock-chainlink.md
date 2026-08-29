# `magicblock-chainlink`

## Purpose

`magicblock-chainlink` is the validator's base-chain account synchronization crate. It is the bridge between Solana RPC/pubsub state and the validator's local `AccountsDb`.

At a high level it:

- fetches accounts from the base layer when RPC reads or transaction submission need them locally,
- subscribes to base-layer account/program updates and turns those updates into local clone operations,
- resolves delegation records for DLP-owned accounts and rewrites local account metadata so delegated accounts execute under their original owners,
- keeps local copies fresh while avoiding duplicate concurrent fetches/clones,
- handles program-account loading, associated-token/eATA projection, post-delegation action dependencies, and undelegation tracking,
- uses the engine's account cache for missing-load coordination and readonly
  account eviction, then releases the corresponding remote subscriptions.

This crate prepares local state for execution. It does **not** decide final post-execution write validity; the processor/SVM path still enforces MagicBlock writable-account invariants.

Chainlink is on the account-availability hot path for RPC reads and transaction submission. Changes must preserve low-latency fetch/clone behavior, bounded subscription overhead, deduplication, and low contention. Do not introduce avoidable duplicate remote fetches/clones, subscription churn, blocking work, excessive logging, or heavy per-account allocations/serialization; call out any unavoidable performance tradeoff explicitly.

## Update requirement

Whenever behavior in `magicblock-chainlink` changes, or another crate changes Chainlink flows, queue an update to this document for the weekly documentation-maintenance task for changes to:

- account fetch/clone classification,
- delegation-record resolution or local delegated/confined/undelegating flags,
- subscription ownership, engine cache eviction, reconnection, or update ordering,
- program loading,
- ATA/eATA projection,
- post-delegation action dependency handling,
- lifecycle-mode behavior,
- public APIs used by `magicblock-api`, `magicblock-aperture`,
  `magicblock-accounts`, or `programs/magicblock`,
- tests or validation commands relevant to this crate,
- performance characteristics of fetch/clone, deduplication, subscription, cache eviction, or update-ordering paths.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

Primary source files:

| Path | Role |
|---|---|
| `magicblock-chainlink/src/lib.rs` | Crate exports. Re-exports Chainlink types and `AccountFetchContext`. |
| `magicblock-chainlink/src/chainlink/mod.rs` | Public Chainlink facade, replication-mode wrapper, transaction/account ensure entrypoints, stale-account cleanup, and engine-eviction listener. |
| `magicblock-chainlink/src/chainlink/fetch_cloner/` | Main fetch/clone pipeline, delegation handling, subscription-update processing, ATA/eATA projection, and clone deduplication. |
| `magicblock-chainlink/src/remote_account_provider/` | RPC/pubsub provider, subscription ownership/tracking, websocket/gRPC clients, and program-account resolution. |
| `magicblock-chainlink/src/submux/` | Multiplexes multiple pubsub clients, deduplicates/debounces updates, reconnects clients, fans updates into one stream. |
| `magicblock-chainlink/src/cloner/mod.rs` | Request types and the concrete Engine account/program materialization operations. |
| `magicblock-chainlink/src/testing/` | Test support behind `dev-context`. |
| `magicblock-chainlink/tests/` | Integration-style Chainlink tests for account ensure, delegation, redelegation, ordering, and race recovery. |

Main consumers:

- `magicblock-api` constructs the production Chainlink stack during validator startup.
- `magicblock-aperture` uses Chainlink for RPC read misses and transaction submission account availability.
- `magicblock-accounts` uses Chainlink/account cloning glue for account-manager flows and scheduled commit integration.
- `programs/magicblock` uses `dev-context` Chainlink helpers in tests and validator-only program flows.

## Main public types and APIs

### Chainlink facade

`src/chainlink/mod.rs` defines the main stack:

- `InnerChainlink<T, U>`: active Chainlink implementation parameterized by RPC
  and pubsub clients and backed by a concrete `Engine`.
- `ReplicationModeAwareChainlink<T, U>`: wrapper with `Enabled` and `Disabled` modes.
- `ProdInnerChainlink` / `ProdChainlink`: production aliases using
  `ChainRpcClientImpl`, `SubMuxClient<ChainUpdatesClient>`, and `Engine`.

Important methods:

- `try_new_from_endpoints(...)`: builds `RemoteAccountProvider`, `FetchCloner`, risk service, and subscription update channel from configured base-layer endpoints.
- `ensure_transaction_accounts(tx)`: ensures all transaction account keys, plus a possible fee-payer ephemeral balance PDA, are present locally. No-op system transfers are skipped.
- `ensure_accounts(pubkeys, mark_empty_if_not_found, fetch_context)`: fetches/clones accounts but returns only fetch/clone status.
- `fetch_accounts(pubkeys, fetch_context)`: ensures accounts and then reads them from the local bank.
- `accounts_delegated_on_base_and_er(pubkeys, fetch_context)`: checks that each account is DLP-owned on base and represented as delegated/DLP-owned locally.
- `account_delegation_statuses(pubkeys, fetch_context)`: returns base-layer delegation plus explicit account-on-ER status (`missing`, `delegated`, or `not_delegated`) for owner-program undelegation request logs.
- `undelegation_requested(pubkey)`: called by committor/account flows before an account is undelegated so Chainlink keeps watching for base-layer completion.
- `fetch_undelegation_requests()`: scans base-layer Delegation Program accounts for active `UndelegationRequest` PDAs using filtered `getProgramAccounts` and returns decoded `ObservedUndelegationRequest`s for `magicblock-accounts`.
- `fetch_count()` / `is_watching()`: mainly observability/testing helpers.

Disabled replication mode is intentionally conservative:

- `ensure_accounts` is a no-op success.
- `fetch_accounts` returns `None` for each requested account.
- `ensure_transaction_accounts` errors with `DisabledForNonPrimaryMode`.
- undelegation tracking is ignored.

### Engine materialization

`src/cloner/mod.rs` defines the requests and operations used to materialize
remote state through Engine:

- `AccountCloneRequest` carries `pubkey`, resolved `AccountSharedData`, optional `commit_frequency_ms`, post-delegation `DelegationActions`, and optional `delegated_to_other` authority.
- `DelegationActions` wraps post-delegation action instructions from delegation records.
- `clone_account`, `clone_program`, and `evict_account` apply that state through
  the concrete Engine owned by Chainlink.

Chainlink constructs the desired complete account image with `AccountBuilder`;
it does not apply field patches or mutate Engine-owned account images in place.
Engine materializes that complete image through
`Engine::account(...).create/update`, and Engine alone composes and applies the
MagicRoot field patches.
Ensure/fetch paths use `create`; subscription refreshes require the account to
already exist and use `update`. The same distinction applies to executable
program accounts. Only creation may carry post-delegation actions. An update
that unexpectedly carries actions fails closed, and a subscription update for
an absent target is ignored instead of creating it outside the ensure path.
MagicRoot is the final slot-ordering boundary: older slots fail, equal slots
require a genuine mode change earlier in the same patch transaction, and a
same-mode duplicate fails. `AccountFieldPatch::sequence` therefore applies mode
before slot, while `AccountSharedData::set_mode` marks mode dirty only when the
value actually changes. Chainlink still owns whether a requested lifecycle
transition is valid; MagicRoot's generic mode-transition allowance is not a
substitute for delegation/undelegation resolution.

## Runtime flow: transaction account ensure

`ensure_transaction_accounts` performs the normal transaction-preparation flow:

1. Skip no-op system transfer transactions (`filters/noop_system_transfer.rs`).
2. Collect all account keys from the sanitized transaction.
3. Derive `ephemeral_balance_pda_from_payer(fee_payer, 0)` and add it if absent locally.
4. Mark all collected pubkeys as `mark_empty_if_not_found`; missing transaction accounts are cloned as empty placeholders when appropriate.
5. Call `ensure_accounts` with `AccountFetchContext::send_transaction(signature)`.
6. `ensure_accounts` calls `FetchCloner::fetch_and_clone_accounts_with_dedup`; the fetcher uses `Engine::accounts().ensure(...)` to promote cached hits and reserve missing loads.

Pitfalls:

- This method only ensures availability. It must not loosen execution access rules.
- `mark_empty_if_not_found` is broad for transaction submission by design; changing it can affect how missing fee-payer/escrow/transaction accounts appear to execution.
- The fee-payer balance PDA logic must stay aligned with Magic Program ephemeral balance handling.

## Runtime flow: fetch and clone pipeline

The central implementation is `FetchCloner::fetch_and_clone_accounts_with_dedup` and its inner `fetch_and_clone_accounts`.


### Fetch attribution

Chainlink must preserve parent entrypoint while replacing `fetch_reason` for internal follow-up work such as delegation records, program data, post-delegation action dependencies, undelegating refreshes, subscription-update clones, and ATA projection.

### Keeper coordination and bank fast path

Before fetching remotely:

1. Blacklisted accounts are filtered out.
2. Existing non-undelegating accounts in `AccountsDb` are treated as ready.
3. Existing undelegating accounts are checked asynchronously by `should_refresh_undelegating_in_bank_account` to see whether base-layer undelegation completed.
4. `Engine::accounts().ensure(...)` promotes existing cached accounts and returns either an `AccountLoad` reservation or `AccountWait` handle for each missing account.

Only the caller holding `AccountLoad` fetches and clones a missing account. Other callers await `AccountWait`. A successful create returns the exact `AccountMode` submitted to `Engine::account(...).create(...)`; the ensure owner calls `AccountLoad::complete(mode)` only for an account materialized by its own batch. Keeper admits non-mutable modes to recency/eviction, while delegated, transient, and ephemeral modes remain untracked. A skipped, absent, or failed materialization drops the guard and reports failure to waiters.

This engine-owned reservation is the only missing-account deduplication layer. Preserve it for request-driven ensure operations: bypassing it can amplify RPC traffic, clone transactions, and transaction-submission latency.

Clone paths that already have resolved state and do not call
`Engine::accounts().ensure(...)`—including normal account/program subscription
updates and airdrops—must not affect Keeper's LRU. Subscription paths update
only accounts already present in the bank; greedy discovery explicitly enters
the ensure path when a missing delegated account needs initial creation. These
paths rely on Engine scheduler account locks for serialization and on MagicRoot
slot/mode validation for stale or duplicate outcomes. Chainlink must not add a
parallel pending-clone mutex, waiter map, or ownership guard.

Clone lifecycle metrics are emitted through `chainlink_clone_accounts_total` using bounded enum labels only. Submitted clone calls record success/failure outcomes; local account/program fast-path skips and program-allowlist skips record `outcome=skipped`. If the remote fetch fails before a concrete clone request exists, Chainlink records one skipped lifecycle event per requested pubkey with `remote_result=failed` and `clone_intent=unknown`. These counters must never use pubkeys, signatures, owner pubkeys, raw errors, or other unbounded/user-controlled values as labels.

Empty placeholders are created in `RemoteAccountProvider::try_get_multi` when
RPC returns `None` and the pubkey is included in `mark_empty_if_not_found`; the
provider converts the missing account into a zero-lamport, default-owner,
empty-data account with `AccountMode::Placeholder` and emits
`converted_to_empty`. Placeholder clone stages (`clone_submitted`,
`clone_submit_failed`, `observed_in_bank_after_ensure`, and
`still_missing_after_ensure`) are emitted only when the account clone request
has that exact empty-placeholder shape. The `later_refetched` stage is
deliberately not emitted yet because detecting repeated same-pubkey placeholders
with retained pubkey state would add unbounded memory/cardinality risk; use
group 7 sketches or sampled logs for repeated-same-pubkey detection instead.

### Remote fetch

`RemoteAccountProvider::try_get_multi` subscribes before fetching so subscription updates that arrive during the fetch can win over stale RPC data. It:

1. Claims entries in `fetching_accounts` for pubkeys not already being fetched.
2. Sets up direct account subscriptions for claimed pubkeys.
3. Starts an RPC fetch with `min_context_slot` equal to the observed chain slot or requested slot.
4. Waits for either RPC results or a subscription update that is at least as new as the fetch start slot.
5. Returns results in input order.

The lower pending-fetch dedup layer records `chainlink_pending_fetch_accounts_total`, `chainlink_pending_fetch_waiters_total`, `chainlink_pending_fetch_waiters_gauge`, and `chainlink_pending_fetch_owner_duration_seconds` with `layer="remote_account_provider"`. Claimed pubkeys record `owned`; calls that join existing `fetching_accounts` work record `joined_existing`, waiter total, and active waiter gauge. Subscription-update wins record `resolved_by_subscription_update`, while late RPC completions after such a win or replacement record `rpc_fetch_completed_after_update`. `FetchingAccountState` stores bounded metric metadata (`AccountFetchContext` and owner start time) so subscription-update completion preserves the original entrypoint/fetch reason without adding pubkey/signature labels.

This pending-fetch instrumentation does not change fetch/clone behavior, dedup ownership, subscription ordering, or remote-fetch retry behavior; it only records counters, gauges, and histograms on existing control-flow edges.

Companion-account slot-match fetches are instrumented by `chainlink_companion_fetch_attempts` and `chainlink_companion_fetch_duration_seconds` with labels `entrypoint`, `fetch_reason`, `companion_kind`, and `outcome`. `companion_kind` is a bounded label (`program_data`, `delegation_record`, `ata_projection`) that describes the slot-consistent relationship being resolved and is distinct from `fetch_reason`. These metrics are emitted from `RemoteAccountProvider::try_get_multi_until_slots_match` and must not change retry behavior, `min_context_slot`, slot matching, or subscription cleanup behavior. Labels must never include pubkeys, signatures, raw errors, endpoints, owners, or program IDs.

Companion-account fetch failures emit a standardized `error!` log with the message `Failed to fetch companion account`. The structured log includes the primary account pubkey, companion account pubkey, companion kind, origin entrypoint and reason from `AccountFetchContext`, context slot, and error. This applies to both subscription-update and non-subscription companion fetch origins. Expected optional companion absence, such as an eATA fetch that succeeds as `NotFound`, must not be logged as an error. This logging must not change retry behavior, slot matching, `min_context_slot`, clone/drop decisions, or subscription cleanup behavior.

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
   - confined accounts (`authority == Pubkey::default()`) become zero-lamport
     `AccountMode::Ephemeral`,
   - accounts assigned to this validator become `AccountMode::Delegated`,
     except raw eATA PDAs are not marked delegated directly,
   - every other zero-lamport account created locally becomes
     `AccountMode::Placeholder`,
   - every remaining account becomes `AccountMode::ReadOnly`,
   - `commit_frequency_ms` is included only for accounts delegated/confined to this validator.
6. If authority belongs to another validator, `delegated_to_other` is set on the clone request.
7. Missing non-internal delegation records are reported in `FetchAndCloneResult::missing_delegation_record`.

Important caveats:

- Invalid delegation records are fatal for the fetch/clone operation because local ownership would be ambiguous.
- Post-delegation actions are parsed/decrypted only when the record authority is this validator.
- Confined accounts are local-only scratch state: Chainlink discards their
  base-layer lamports and materializes them as zero-lamport `Ephemeral`.
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

After those checks, Chainlink passes the actions to
`Engine::account(pubkey).create(...)`. The engine composes account
materialization and MagicRoot `PostFinalize` into one transaction, so the
actions run only after the delegated account is finalized. Subscription updates
use `update(...)` without actions; actions can never be replayed by a refresh.
There is no separate MBV post-delegation executor builtin.

Do not execute or ignore these actions blindly. They are part of clone-time invariants for post-delegation behavior.

### Program account resolution

Executable accounts are converted into `LoadedProgram` values and materialized
through Engine.

Supported loader handling lives in `remote_account_provider/program_account.rs`:

- Loader V1: deprecated; subscription updates for V1 are unexpected.
- Loader V2: single account contains metadata/data.
- Loader V3: program account plus separate program-data account; Chainlink fetches both with matching slots and holds a `ProgramData` subscription reason while resolving.
- Loader V4: single account with loader-v4 state and deployable data handling.

Loader V3 state and Loader V4 instructions retain upstream serde/bincode
encoding because those external loader types do not provide wincode schemas.
Other supported fixed Solana payloads in this crate use wincode.

Program clone restrictions:

- `allowed_programs` from config, when non-empty, limits program cloning.
- native loader accounts should be blacklisted and are not cloned.
- LoaderV3 program-data subscriptions must be released on success and error paths.

### ATA/eATA projection

Chainlink has special handling for associated token accounts and ephemeral ATAs:

- Base ATAs are recognized via `magicblock_core::token_programs::is_ata`.
- For each ATA, Chainlink derives the companion eATA PDA with `try_derive_eata_address_and_bump`.
- It subscribes to both ATA and eATA using `SubscriptionReason::AtaProjection`.
- Projection requires a valid, slot-matched delegation record whose authority is this validator and whose owner is `EATA_PROGRAM_ID`. When those checks pass and the eATA can be projected, Chainlink clones a projected delegated ATA into the local bank.
- Raw eATA program-subscription updates without existing local projection interest are routed through greedy discovery rather than dropped. Greedy discovery can validate the eATA delegation record, fetch the remote base ATA, project the local ATA, and preserve post-delegation actions on the projected clone request.
- The non-greedy projection helper may still avoid companion fetches for a program-source raw eATA update when no delegation record is already supplied and no local projection interest exists (a watched ATA/eATA projection subscription, a watched raw eATA, or a supported base ATA already present locally). This local-interest gate does not disable the separate greedy-discovery path.
- Projection preserves the base ATA's owner and data length, which is important for Token-2022 extensions.
- Missing eATAs can be remembered in `known_empty_eatas`, but only after confirmed `NotFound` while an eATA subscription is live.
- Raw eATA PDAs are not marked delegated directly; their state is projected into the corresponding base ATA.
- Explicit RPC/transaction ensure paths still resolve eATA delegation records and project delegated ATAs normally when the requested account requires it; the local-interest narrowing applies to program-subscription firehose updates.

Pitfalls:

- Do not rebuild Token-2022 accounts as legacy SPL Token accounts; use the projection helpers that preserve layout.
- Native-token normalization is safe only after Chainlink has proved the cloned account is a canonical ATA/eATA projection target. Non-canonical delegated wrapped-SOL token accounts must be preserved because commit settlement will not remap them to eATA.
- If canonical delegated ATA normalization reports malformed token-program data,
  reject the clone request instead of forwarding the unnormalized account to
  Engine.
- Projected ATAs are virtual eATA views and should be uncloseable locally; do not preserve base close authority on the projected clone.
- Chainlink's current subscription prefilter admits a same-slot delegated refresh only when it replaces plain/undelegating local state. The downstream MagicRoot boundary is generic and admits any genuine mode transition at the same slot, supporting transitions such as `Transient -> ReadOnly`, `ReadOnly -> Delegated`, and a future `Ephemeral -> Delegated`. Neither rule means same-slot re-delegation to the same validator is fully supported; without a delegation generation/index, `account_still_undelegating_on_chain` cannot distinguish `delegation_slot == remote_slot_in_bank` from a still-pending undelegation, and `magicblock-chainlink/tests/07_redeleg_us_same_slot.rs` remains ignored for that reason.
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
- Program-subscription updates are allowed even if the pubkey is not in the direct-account LRU, but DLP-owned program updates are preclassified before any delegation-record or other companion fetch.
- Greedy discovery is always enabled for absent or unwatched delegated accounts discovered through DLP program-subscription updates. Ordinary non-internal DLP-owned user-account updates and raw eATA updates without local projection interest therefore reach greedy discovery so Chainlink can resolve delegation authority and preserve post-delegation actions.
- Existing local delegated non-undelegating accounts are authoritative. DLP program updates for them clean up direct subscriptions and must not fetch a delegation record, clone, or overwrite local state.
- Existing local undelegating accounts bypass the internal-DLP early drop and continue undelegation completion/redelegation processing so completion remains observable.
- The current Chainlink subscription prefilter ignores non-advancing updates unless they represent a same-slot delegated refresh needed for undelegate/redelegate recovery. Materialized clone transactions are additionally subject to MagicRoot's generic slot/mode guard.
- Delegated updates cause direct subscription cleanup; undelegation-completion updates retain/directly ensure subscriptions as appropriate and release `UndelegationTracking` ownership.

### DLP undelegation request scanning

Owner-program undelegation requests are discovered in two ways:

- Live updates: DLP-owned `UndelegationRequest` account subscription/program-subscription updates are decoded in `FetchCloner::process_subscription_update` and broadcast as `ObservedUndelegationRequest`.
- Backfill scans: `FetchCloner::fetch_undelegation_requests` calls `getProgramAccounts` for `dlp_api::id()` with a `DataSize(UndelegationRequest::size_with_discriminator())` filter and a discriminator `memcmp` at offset `0`, then decodes each returned account with `UndelegationRequest::try_from_bytes_with_discriminator`.

The scan uses Base64Zstd account encoding and gets a nearby base-chain slot for `observed_slot`. Malformed matching accounts are logged and skipped; a bad account must not abort the whole scan. Polling cadence is controlled by `chainlink.undelegation-request-poll-interval` in `magicblock-config` and consumed by `magicblock-accounts`.

### Greedy discovery

For DLP-owned program-subscription firehose updates, Chainlink first classifies the pubkey using local bank state, direct-watch state, and ATA/eATA projection interest.

Greedy discovery is always enabled for absent or unwatched delegated accounts discovered through DLP program-subscription updates. This preserves clone-time post-delegation action execution for new delegations discovered from the DLP program subscription. The prefilter may skip delegation-record resolution only for updates already resolved as locally authoritative or for genuine internal DLP records/metadata/commit state; it must not reject an absent account merely for lacking local interest.

Raw eATA updates with no existing local projection interest also enter greedy discovery. That path validates a slot-matched delegation record with this validator as authority and `EATA_PROGRAM_ID` as owner, fetches the remote base ATA, projects the delegated local ATA, and carries any post-delegation actions. Separately, the non-greedy projection helper retains its local-interest gate and may avoid companion fetches for a program-source raw eATA when no delegation record was supplied and no ATA/eATA projection interest exists.

Updates for directly watched accounts or locally relevant ATA/eATA projection state may still greedily fetch and clone if the delegation record says the account belongs to this validator (or is confined). Explicit RPC/transaction ensure paths are not narrowed by this prefilter: they still fetch delegation records and clone delegated accounts normally.

Updates delegated to other validators are ignored after discovery so this validator does not clone state it cannot execute against.

### Internal DLP update filtering and collision sighting

DLP-owned program-subscription updates are classified before internal-DLP payload filtering or collision handling. Updates for existing local delegated non-undelegating accounts clean up direct subscriptions and return without overwriting locally authoritative state. Updates for existing local undelegating accounts and locally relevant ATA/eATA projections continue past the internal-DLP early drop so undelegation completion and projection processing can run.

Program-subscription updates whose payload parses as an internal DLP account (delegation record, delegation metadata, commit record, program config) are then dropped in `FetchCloner::process_subscription_update` **before** greedy discovery, with zero remote fetches — their derived "record of a record" PDA never exists, and these updates dominate the DLP program-subscription firehose.

The internal-DLP fast path must not classify ordinary non-internal user-account updates as irrelevant merely because they are absent locally; those updates must fall through to greedy discovery so valid same-slot delegation records can trigger clone and post-delegation action handling.

The exception is a delegated account whose app data byte-collides with an internal DLP discriminator (LE u64 100–103). Such accounts must still reach greedy discovery so their post-delegation actions execute. `DlpCollisionTracker` (single lock, so check-then-park is atomic against sight-then-release) resolves this without a fetch:

- Every delegation-record-shaped update records a monotonic sighting (`record pubkey -> max slot`), from either subscription source. Only program-subscription updates are dropped/parked; account-subscription updates always continue into normal processing.
- An internal-looking account update whose derived delegation-record PDA was sighted at or after its own slot proceeds to greedy discovery (a fresh delegation writes both accounts in one slot).
- Otherwise the update is parked (pubkey + slot, keyed by its derived record PDA); a later record sighting releases it into an authority-gated, deduped fetch+clone that replaces an undelegating bank copy only when the record proves a newer delegation generation.
- Genuine internal PDAs always miss the sighting cache and are dropped. A missed sighting degrades to lazy on-demand cloning via the normal getAccount/send-transaction paths — never to incorrect state.

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

- `DirectAccount`: normal account monitoring.
- `DelegationRecord`: temporary/explicit monitoring for delegation record PDAs.
- `ProgramData`: LoaderV3 program-data accounts.
- `UndelegationTracking`: protected monitoring while an account is expected to complete undelegation on base.
- `AtaProjection`: ATA/eATA projection monitoring.

Ownership is reference-counted per reason. Releasing one reason does not unsubscribe while other reasons remain.

`ensure_subscription` differs from `acquire_subscription`: it does not increment an already-held reason. This is used by eATA projection to retain monitoring without unbounded refcount growth.

Registration outcome metrics (`chainlink_subscription_registration_accounts_total`, exported as `mbv_chainlink_subscription_registration_accounts_total`) are emitted once per claimed subscription attempt by entrypoint, fetch reason, subscription reason, and terminal registration outcome. Waiter-only fetch callers do not independently set up subscriptions and are not counted separately; direct `try_get_multi` owners preserve their `AccountFetchContext`, while callers without a fetch context use `entrypoint="internal", fetch_reason="requested_account"`.

Release and cleanup outcome metrics (`chainlink_subscription_release_accounts_total{reason,outcome}` and `chainlink_subscription_cleanup_accounts_total{cleanup_source,outcome}`, exported with the `mbv_` prefix) are emitted only on cold subscription release/cleanup transition paths, never on per-update hot loops. Release metrics classify each explicit `release_subscription_with_mode` / silent delegated-account release result (`unsubscribed`, `already_absent`, `unsubscribe_failed`, `retained_intentionally`, `retained_other_reasons`). Cleanup metrics classify the actual unsubscribe action by `cleanup_source` (`normal_release`, `manual_unsubscribe`, `delegated_account_silent`, `reconciler`) and `outcome` (`unsubscribed`, `already_absent`, `unsubscribe_failed`, `removal_update_failed`, `retained_intentionally`). All labels are static/enum values only; no pubkey, signature, raw error, or endpoint labels are used.

### Engine cache eviction and stale-account cleanup

The engine owns account recency, capacity, missing-load reservations, and eviction selection. Chainlink subscribes to `Engine::accounts().subscribe_evictions()`. For each evicted readonly account it releases remote subscription ownership and submits `Cloner::evict_account`; mutable accounts are ignored defensively. The provider's `SubscribedAccounts` is an unbounded presence set for subscriptions Chainlink still owns, not a second cache.

A separate stale-account channel is retained for exceptional subscription loss or a late account update after its subscription was released. That path rechecks same-pubkey subscription state under the provider lock before submitting a defensive eviction, preventing an old notification from removing an account that has already been watched again.

### Reconciliation

If subscription metrics are enabled, a background task periodically runs `subscription_reconciler::reconcile_subscriptions` to compare the tracked subscription set with actual pubsub-client subscriptions and repair drift. A tracked subscription that cannot be restored is removed from tracking and routed through stale-account cleanup; an extra remote-only subscription is simply unsubscribed because normal bank removal belongs to engine eviction.

When the pubsub client is `SubMuxClient`, reconciliation snapshots are intentionally based only on currently connected inner clients. Disconnected/reconnecting clients are ignored by `subscriptions_union()` and `subscriptions_intersection()` until the reconnect path has reconnected them, resubscribed programs/accounts from the authoritative trackers, performed its catch-up pass, and marked them connected again. Reconciler-triggered SubMux subscribe/unsubscribe repair operations also fan out only to connected clients; reconnecting clients catch up through the reconnect path instead. If no inner pubsub client is connected, reconciliation skips repair/noisy tracking-vs-pubsub mismatch reporting for that tick because there is no live client to inspect or repair.

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

`ChainlinkConfig` wraps `RemoteAccountProviderConfig` and includes settings such as `remove_confined_accounts`, allowed program filters, resubscription delay, Range risk checks, and `undelegation_request_poll_interval` for the DLP request backfill consumer in `magicblock-accounts`.

`RemoteAccountProviderConfig` includes:

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
- Because subscription/fetch updates are driven by external base-layer events and untrusted submissions, treat engine load coordination/cache eviction, slot matching, ordering, and subscription ownership as security controls against races, stale overwrite, and resource exhaustion. Do not relax them for performance.

Preserve these invariants when editing this crate:

1. **Never clone DLP-owned state as writable delegated state without a valid delegation record**, except explicitly recognized internal DLP accounts.
2. **Delegated local accounts must be presented with their original owner**, not the Delegation Program owner.
3. **Authority matters**: this validator can mark accounts delegated only when the record authority is this validator or the confined/default authority.
4. **Chainlink may materialize only `Delegated`, `Placeholder`, `Ephemeral`, or
   `ReadOnly` accounts. Confined accounts are zero-lamport `Ephemeral`; newly
   materialized zero-lamport accounts are `Placeholder`; all other engine modes
   must fail at the Chainlink boundary.**
5. **Mutable local accounts are excluded from engine cache tracking and are protected from defensive bank eviction.**
6. **Subscription update ordering must not overwrite fresher local state with older or duplicate data.** Chainlink currently has a narrow same-slot delegated-refresh exception; MagicRoot independently rejects every older replacement and permits an equal-slot replacement only when its account mode genuinely changes.
7. **Fetches that need companion accounts must use matching slots or a minimum context slot** so account and delegation/program-data records are coherent.
8. **Engine load reservations must complete with the exact successfully created mode or drop their guards**, and direct clone paths must never interact with Keeper's LRU.
9. **Program-data subscriptions for LoaderV3 must be cleaned up on all paths.**
10. **ATA/eATA projection must preserve base ATA layout and token-program ownership.**
11. **Post-delegation action dependencies must be available before the engine composes account creation and `PostFinalize` into one transaction.**
12. **Disabled/non-primary mode must not perform remote fetches or transaction account ensures.**
13. **This crate must not weaken processor/SVM access validation.** It only prepares local account state.
14. **Fetch/clone and subscription paths must remain performance-conscious.** Preserve deduplication, bounded waiting, engine cache ownership, low subscription churn, and non-blocking behavior unless a documented correctness requirement forces a tradeoff.

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

### Cache/eviction bugs

Start with:

- `AccountsBank::ensure`,
- `AccountLoad::complete`,
- `InnerChainlink::subscribe_account_evictions`,
- `RemoteAccountProvider::unsubscribe`,
- `RemoteAccountProvider::evict_unwatched_with_subscription_lock`.

Do not track or evict mutable local state.

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
- Useful Chainlink test files: `magicblock-chainlink/tests/basics.rs`, `01_ensure-accounts.rs`, `03_deleg_after_sub.rs`, redelegation tests `04` through `07`, `08_subupdate-ordering.rs`, and `09_waiter_reconciliation_race.rs`.
- Performance validation intent: fetch/clone, subscription, cache eviction, or update-ordering hot-path changes should include the smallest practical test or measurement that can expose duplicate fetches/clones, increased latency, contention, or subscription churn; if skipped, report the residual performance risk.

## Adjacent implementation references

- `../engine/engine/src/accessor.rs` — concrete account operations used to
  materialize and evict local state.
- `.agents/context/crates/magicblock-accounts.md` — scheduled commit integration and undelegation notification consumer.
- `.agents/context/crates/magicblock-aperture.md` — RPC read and transaction submission account-ensure caller.
- `.agents/context/crates/magicblock-aml.md` — signer risk-check integration for post-delegation actions.
- `magicblock-chainlink/src/cloner/mod.rs` — `AccountCloneRequest`,
  `DelegationActions`, and concrete Engine materialization operations.
- `magicblock-chainlink/src/chainlink/fetch_cloner/` — fetch/clone pipeline, delegation handling, ATA/eATA projection, and engine load coordination.
- `magicblock-chainlink/src/remote_account_provider/` — RPC/pubsub provider, subscription ownership/tracking, and program-account resolution.
- `magicblock-chainlink/tests/` — Chainlink account ensure, delegation, ordering, and race-recovery tests.
