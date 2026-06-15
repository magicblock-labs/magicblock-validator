# `magicblock-accounts`

## Purpose

`magicblock-accounts` is the validator-side glue between Magic Program scheduled intents, Chainlink account lifecycle tracking, and the committor service. In the current implementation it does **not** own the account-cloning manager described by its historical README; account fetching/cloning is owned by `magicblock-chainlink` and `magicblock-account-cloner`. This crate's active runtime responsibility is scheduled commit processing.

At a high level it:

- exposes the `ScheduledCommitsProcessor` trait used by `magicblock-api`'s slot ticker;
- implements `ScheduledCommitsProcessorImpl`, which drains accepted `ScheduledIntentBundle`s from the Magic Program global transaction scheduler;
- forwards scheduled and recovered intent bundles to `magicblock-committor-service`;
- tracks per-intent metadata needed to signal `ScheduledCommitSent` back into local validator execution after base-layer intent execution finishes;
- notifies Chainlink when accounts are about to be undelegated so base-layer undelegation completion can be watched;
- starts a one-shot recovery pass for persisted pending intent bundles after ledger replay and account-bank reset.

This crate sits on the commit/undelegation settlement path and interacts with the transaction scheduler. It is not part of ordinary SVM transaction execution, but its ticker-triggered work can affect slot progression, local commit lifecycle state, Chainlink subscription state, and committor throughput. Avoid adding blocking work, unbounded locks, duplicate intent scheduling, or heavy per-intent processing without an explicit performance tradeoff.

## Update requirement

Update this document in the same change whenever behavior in `magicblock-accounts` changes, or when another crate changes the flows/contracts consumed here. This file is useful only if it reflects the current implementation.

Update it for changes to:

- `ScheduledCommitsProcessor` trait methods or `ScheduledCommitsProcessorImpl::new` wiring;
- scheduled intent draining, metadata tracking, result processing, or `SentCommit` construction;
- pending intent recovery timing, delegation checks, or recovered scheduling behavior;
- Chainlink undelegation notification behavior;
- Magic Program scheduled intent types, `TransactionScheduler` global state, `ScheduledCommitSent`, or `SentCommit` fields;
- committor result subscription, broadcast error handling, patched-error/callback-report handling, or execution-output variants;
- startup/shutdown ordering in `magicblock-api` that affects this processor;
- validation commands or integration suites relevant to schedule intents, commits, or undelegation;
- performance characteristics of scheduled commit processing, result handling, or recovery.

## Where it sits in the repository

Primary files and nearby contracts:

| Path | Role |
|---|---|
| `magicblock-accounts/Cargo.toml` | Crate dependencies. Pulls in Chainlink/cloner aliases, committor service, core scheduler types, metrics, and Magic Program scheduled intent types. |
| `magicblock-accounts/README.md` | Historical notes about an `AccountsManager`/`ensure_accounts` design. Treat source and this guide as canonical for current behavior. |
| `magicblock-accounts/src/lib.rs` | Public exports: `config::*`, `traits::*`, `errors`, and `scheduled_commits_processor`. |
| `magicblock-accounts/src/config.rs` | Defines a local `LifecycleMode` enum and `requires_ephemeral_validation`; currently no tracked consumer uses this type. Do not confuse it with `magicblock-config::config::LifecycleMode`, which is the validator config type in use. |
| `magicblock-accounts/src/traits.rs` | Defines the `ScheduledCommitsProcessor` async trait consumed by the API slot ticker. |
| `magicblock-accounts/src/scheduled_commits_processor.rs` | Main implementation: scheduled intent draining, committor scheduling, pending recovery, Chainlink undelegation notifications, result subscription loop, and `SentCommit` signaling. |
| `magicblock-accounts/src/errors.rs` | Error enums and result aliases for scheduled commit processing plus older account/committor error variants. |
| `magicblock-api/src/magic_validator.rs` | Production wiring. Constructs `ScheduledCommitsProcessorImpl`, starts recovery after replay/reset, passes it to the slot ticker, and stops it before stopping the committor service. |
| `magicblock-api/src/tickers.rs` | Slot ticker checks `MagicContext::has_scheduled_commits`, executes `AcceptScheduleCommits`, then calls `ScheduledCommitsProcessor::process`. |
| `programs/magicblock/src/magic_context.rs` | `MagicContext` storage and `has_scheduled_commits` fast check used by the slot ticker. |
| `programs/magicblock/src/schedule_transactions/process_accept_scheduled_commits.rs` | Magic Program instruction that moves scheduled intents from `MagicContext` into global `TransactionScheduler` state. |
| `programs/magicblock/src/magic_scheduled_base_intent.rs` | Defines `ScheduledIntentBundle`, `MagicIntentBundle`, and helpers such as `get_all_committed_pubkeys` / `has_undelegate_intent`. |
| `magicblock-committor-service/src/service.rs` | `BaseIntentCommittor` trait and `CommittorService` oneshot APIs used by this crate. |
| `magicblock-committor-service/src/committor_processor.rs` | Persists and schedules normal intents; loads pending intent bundles for recovery; schedules recovered bundles without re-persisting. |
| `test-integration/test-schedule-intent/` and `test-integration/schedulecommit/` | Integration coverage for schedule intent, commit, and commit-and-undelegate behavior. |

Main consumers:

- `magicblock-api` is the only production consumer of `ScheduledCommitsProcessorImpl`.
- `magicblock-api::tickers` depends on the `ScheduledCommitsProcessor` trait rather than the concrete implementation.
- The committor service is the downstream executor for base-layer intents.
- Chainlink is notified when accounts are entering undelegation so it can watch/update base-layer state.
- The Magic Program is both upstream (schedules/accepts intents) and downstream (receives local `ScheduledCommitSent` signals).

Important boundaries:

- This crate does not fetch or clone transaction accounts for ordinary execution; use `magicblock-chainlink` / `magicblock-account-cloner` for account availability changes.
- This crate does not build base-layer commit transactions; that belongs to `magicblock-committor-service`.
- This crate does not enforce final SVM writable-account access rules; that belongs to the processor/SVM path.

## Public API shape / Main public types and APIs

`magicblock-accounts/src/lib.rs` exports:

```rust
mod config;
pub mod errors;
pub mod scheduled_commits_processor;
mod traits;

pub use config::*;
pub use traits::*;
```

### `ScheduledCommitsProcessor` trait

Defined in `src/traits.rs`:

- `async fn process(&self) -> ScheduledCommitsProcessorResult<()>` drains accepted scheduled intents and hands them to the committor;
- `fn scheduled_commits_len(&self) -> usize` returns the count of accepted intents in Magic Program global scheduler state;
- `fn clear_scheduled_commits(&self)` clears that global scheduler state;
- `fn stop(&self)` cancels processor background work.

The trait is `Send + Sync + 'static` and is used by `magicblock-api/src/tickers.rs` to keep slot ticker code generic.

### `ScheduledCommitsProcessorImpl`

Defined in `src/scheduled_commits_processor.rs` and constructed with:

```rust
pub fn new(
    committor: Arc<CommittorService>,
    chainlink: Arc<ChainlinkImpl>,
    internal_transaction_scheduler: TransactionSchedulerHandle,
    latest_block: impl LatestBlockProvider,
) -> Self
```

Stored state:

- `committor: Arc<CommittorService>` schedules intent bundles and provides result broadcasts / pending persisted intents;
- `chainlink: Arc<ChainlinkImpl>` receives `undelegation_requested(pubkey)` calls before commit-and-undelegate execution;
- `cancellation_token: CancellationToken` stops the result-processing loop;
- `intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>` maps intent IDs to local metadata needed when committor results return;
- `transaction_scheduler: magicblock_program::TransactionScheduler` accesses the Magic Program's global scheduled action store.

Important public method outside the trait:

- `spawn_pending_intents_recovery(self: &Arc<Self>)` starts a one-shot task that loads persisted pending intent bundles from committor storage, filters them through Chainlink delegation checks, and schedules recoverable bundles. It must run only after ledger replay and account-bank reset.

### Type aliases

- `InnerChainlinkImpl = ProdInnerChainlink<ChainlinkCloner>`
- `ChainlinkImpl = ProdChainlink<ChainlinkCloner>`

These encode the current production Chainlink/cloner stack. If Chainlink generic wiring changes, update these aliases and this guide together.

### Errors

`ScheduledCommitsProcessorError` wraps:

- `tokio::sync::oneshot::error::RecvError` when committor oneshot requests fail;
- boxed `CommittorServiceError` from scheduling, recovery, or result-subscription operations.

`AccountsError` still contains broader account/committor/cloner variants from older APIs. Check whether a variant is actually consumed before relying on it for new behavior.

## Runtime flows

### Normal scheduled commit flow

```text
program invokes Magic Program schedule instruction
  -> MagicContext stores ScheduledIntentBundle(s)
  -> slot ticker sees MagicContext::has_scheduled_commits
  -> validator-signed AcceptScheduleCommits transaction runs locally
  -> Magic Program moves bundles into global TransactionScheduler state
  -> ScheduledCommitsProcessorImpl::process drains them
  -> committor service persists/schedules base-layer intent execution
  -> committor broadcasts result
  -> processor registers SentCommit and schedules local ScheduledCommitSent transaction
```

Ordered details:

1. User/program code schedules commit, commit-and-undelegate, commit-finalize, and/or action intent bundles through the Magic Program.
2. `magicblock-api::init_slot_ticker` periodically reads `MAGIC_CONTEXT_PUBKEY` from `AccountsDb` and calls `MagicContext::has_scheduled_commits`.
3. If there are pending scheduled commits, `handle_scheduled_commits` builds `InstructionUtils::accept_scheduled_commits(latest_block.blockhash)` and submits it through the internal `TransactionSchedulerHandle`.
4. `process_accept_scheduled_commits` validates the validator authority signer, drains `MagicContext::scheduled_base_intents`, and calls `TransactionScheduler::default().accept_scheduled_base_intent(...)`.
5. `ScheduledCommitsProcessorImpl::process` calls `take_scheduled_intent_bundles()` on its `magicblock_program::TransactionScheduler` handle. Empty drains are no-ops.
6. For non-empty drains it increments `magicblock_metrics::metrics::inc_committor_intents_count_by` and calls `process_intent_bundles`.
7. `prepare_intent_bundles_for_scheduling` stores `ScheduledBaseIntentMeta` for every intent ID and gathers pubkeys from undelegation intents.
8. `process_undelegation_requests` concurrently calls `chainlink.undelegation_requested(pubkey)` for gathered pubkeys; failures are logged but do not abort the commit.
9. The committor receives the bundles via `CommittorService::schedule_intent_bundles` and returns through a oneshot when scheduling is accepted.
10. The background `result_processor` receives `BroadcastedIntentExecutionResult`s from the committor broadcast channel.
11. `process_intent_result` removes the intent metadata, builds/registers a `SentCommit`, creates or reuses the `ScheduledCommitSent` transaction, encodes it with `with_encoded`, and submits it to the internal scheduler.

Caveats:

- The slot ticker has a TODO about possible delay between accepting and processing scheduled commits. Do not add extra sleeps or slow work to this path.
- `process_undelegation_requests` logs subscription failures and continues. That can leave undelegating accounts in a problematic local state; changing this policy is a lifecycle decision, not a local cleanup.
- `intents_meta_map` is keyed by intent ID. Duplicate IDs or unexpected duplicate results can cause missing metadata and are logged as errors.

### Pending intent recovery flow

```text
validator start/restart
  -> ledger replay
  -> account-bank reset/cleanup
  -> spawn_pending_intents_recovery
  -> committor loads persisted pending bundles
  -> Chainlink verifies accounts delegated on base and ER
  -> recoverable bundles schedule through committor without re-persisting
```

Ordered details:

1. `magicblock-api::MagicValidator::start` processes ledger replay and clears Magic Program global scheduled actions before starting the normal slot ticker.
2. After account-bank reset, and only in the branch where replay/reset is performed, the API calls `processor.spawn_pending_intents_recovery()`.
3. `recover_pending_intents` asks the committor for `get_pending_intent_bundles().await??`.
4. `recoverable_intent_bundles` checks every bundle's committed pubkeys with `chainlink.accounts_delegated_on_base_and_er(&pubkeys, AccountFetchOrigin::GetAccount)`.
5. Bundles with any non-delegated account, or bundles whose checks error, are skipped and logged.
6. Recoverable bundles go through `process_intent_bundles` with `CommittorService::schedule_recovered_intent_bundles`, which schedules without re-persisting rows.
7. If scheduling recovered bundles fails, metadata for those intent IDs is removed to avoid stale entries.

Caveats:

- Recovery must happen after replay/reset because Chainlink delegation checks read local bank state.
- Recovery currently only runs when `MagicValidator::start` enters the replay/reset path. Changing startup branches can affect pending intent durability.
- Filtering requires all committed accounts in a bundle to be delegated on both base and ER; this protects against re-sending stale or already-undelegated work.

### Result-to-local-signal flow

When the committor completes an intent:

1. `BroadcastedIntentExecutionResult` includes the intent ID, success/error output, patched errors, and callback scheduling report.
2. `ScheduledBaseIntentMeta` supplies the original slot, blockhash, payer, committed pubkeys, optional prebuilt `sent_transaction`, and undelegation flag.
3. `build_sent_commit` converts execution output into chain signatures:
   - `ExecutionOutput::SingleStage(signature)` becomes one signature;
   - `ExecutionOutput::TwoStage { commit_signature, finalize_signature }` becomes two signatures;
   - errors try to expose any available commit/finalize signatures through `err.signatures()`.
4. Patched errors and callback scheduling results are stringified into `SentCommit` fields.
5. `register_scheduled_commit_sent(sent_commit)` stores the result for the Magic Program's local `ScheduledCommitSent` processor.
6. The internal transaction scheduler executes the validator-signed `ScheduledCommitSent` transaction so local execution can observe the sent-commit result.

If the original intent did not carry a signed `sent_transaction`, the processor builds a new one with the current latest blockhash. This fallback is important for recovered intents reconstructed from persistence.

### Shutdown flow

`MagicValidator::stop` cancels the validator token, then calls `scheduled_commits_processor.stop()` before `committor_service.stop()`. Preserve this ordering: the result processor must be told to stop while the committor is still available enough to shut down cleanly, and the committor is intentionally stopped last among these services.

## Important internals and caveats

### Magic Program global scheduler state

`magicblock_program::TransactionScheduler::default()` is used as a handle to global scheduled action state. The `AcceptScheduleCommits` instruction writes into that state, and `ScheduledCommitsProcessorImpl::process` drains it. During ledger replay, `magicblock-api` clears this state to avoid re-committing accepted intents replayed from the ledger.

Do not replace this with an ordinary per-instance queue unless the Magic Program and replay semantics are updated together.

### Metadata map and locking

`intents_meta_map` is protected by a standard `Mutex`. Current critical sections are intentionally small: insert metadata and remove metadata by intent ID. Avoid holding this lock across `.await`, committor calls, Chainlink calls, scheduler execution, or expensive logging/formatting.

The code uses `expect(POISONED_MUTEX_MSG)` in the normal processing paths. One recovery cleanup path handles poisoning by logging and taking the inner map. Treat mutex poisoning as a serious invariant violation.

### Undelegation notifications are best-effort

Before scheduling an intent bundle, the processor calls `chainlink.undelegation_requested` for accounts from commit-and-undelegate intents. Errors are aggregated and logged but do not fail scheduling. This favors settlement progress over local watcher correctness; if you change it, document how accounts that are already locally immutable/undelegating recover from missed base-layer subscriptions.

### Historical README/API drift

The README describes `AccountsManager`, `ExternalAccountsManager`, `BankAccountProvider`, `RemoteAccountCloner`, `Transwise`, and `ensure_accounts`. These are not present in the current crate source. Do not implement new features against those names without first checking current Chainlink/cloner/API ownership and updating/removing stale docs.

### Local `LifecycleMode` drift

`magicblock-accounts/src/config.rs` defines a `LifecycleMode` separate from `magicblock-config::config::LifecycleMode`. Current repository usages of validator lifecycle mode use `magicblock-config`, not this local type. Avoid adding new configuration wiring through the local enum unless that duplication is intentional and documented.

## Important invariants

1. `process()` must drain only accepted scheduled intent bundles from Magic Program global scheduler state; it must not re-read `MagicContext` directly.
2. `AcceptScheduleCommits` must be executed before `process()` so MagicContext intents are moved and cleared atomically by the Magic Program.
3. Intent metadata must be inserted before committor scheduling so result processing can build a correct `SentCommit`.
4. Intent metadata must be removed on result handling and on failed recovered scheduling; stale entries can make later duplicate IDs or results misleading.
5. Recovery must run only after ledger replay and local account-bank reset, because delegation checks depend on current local account state.
6. Recovered bundles must be filtered so all committed accounts are delegated on base and ER before scheduling.
7. Recovered bundles must use the committor recovered scheduling path, not the normal persistence path, to avoid duplicating persisted rows.
8. Commit-and-undelegate intents must notify Chainlink before scheduling whenever possible so base-layer undelegation completion can be tracked.
9. The result processor must not block indefinitely on slow local scheduler work without observing cancellation between results.
10. Broadcast lag from committor results is unexpected and requires investigation; silently dropping lagged results would leave local sent-commit state incomplete.
11. `SentCommit` fields must stay aligned with Magic Program `ScheduledCommitSent` expectations, including chain signatures, patched errors, callback reports, included pubkeys, payer, slot, blockhash, and undelegation flag.
12. Documentation and code must not describe this crate as the active account ensure/cloning owner unless that behavior is restored in source.

## Common change areas and what to inspect

### Changing scheduled commit acceptance or slot ticker behavior

Inspect first:

- `magicblock-api/src/tickers.rs` (`init_slot_ticker`, `handle_scheduled_commits`);
- `programs/magicblock/src/magic_context.rs` (`has_scheduled_commits`, `take_scheduled_commits`);
- `programs/magicblock/src/schedule_transactions/process_accept_scheduled_commits.rs`;
- `magicblock-accounts/src/scheduled_commits_processor.rs::process`.

Risks:

- accepting without processing can leave intents stranded in global scheduler state;
- processing without accepting misses MagicContext-staged intents;
- replay can re-populate global scheduled actions unless startup clear semantics are preserved.

### Changing committor scheduling or result handling

Inspect first:

- `ScheduledCommitsProcessorImpl::process_intent_bundles`;
- `prepare_intent_bundles_for_scheduling`;
- `result_processor`, `process_intent_result`, and `build_sent_commit`;
- `magicblock-committor-service/src/service.rs` `BaseIntentCommittor` methods;
- `magicblock-committor-service/src/intent_execution_manager/intent_execution_engine.rs` result fields.

Risks:

- missing result subscriptions can prevent local `ScheduledCommitSent` signaling;
- new `ExecutionOutput` variants must be converted into `SentCommit.chain_signatures` deliberately;
- callback or patched-error fields are user/operator-visible through Magic Program result state.

### Changing undelegation lifecycle behavior

Inspect first:

- `prepare_intent_bundles_for_scheduling` and `process_undelegation_requests`;
- `ScheduledIntentBundle::get_undelegate_intent_pubkeys` / `has_undelegate_intent`;
- `magicblock-chainlink::undelegation_requested`;
- Magic Program code that marks accounts undelegating/immutable when scheduling commit-and-undelegate.

Risks:

- missing Chainlink notification can leave local state out of sync with base-layer undelegation;
- failing the whole commit on notification errors may affect settlement availability;
- ordinary ER execution must not continue mutating accounts after commit-and-undelegate has been scheduled.

### Changing pending intent recovery

Inspect first:

- `spawn_pending_intents_recovery`, `recover_pending_intents`, `recoverable_intent_bundles`, and `process_recovered_intent_bundles`;
- `magicblock-api/src/magic_validator.rs` startup ordering around replay/reset;
- `magicblock-committor-service/src/committor_processor.rs::pending_intent_bundles`;
- Chainlink `accounts_delegated_on_base_and_er` behavior.

Risks:

- running recovery before local bank repair can schedule invalid or stale commits;
- skipping delegation checks can re-send intents for accounts no longer delegated to this ER;
- normal scheduling can duplicate persistence rows for recovered work.

### Cleaning up stale account-manager remnants

Inspect first:

- `magicblock-accounts/README.md`;
- `src/config.rs` and `src/errors.rs` for unused historical APIs;
- `agents/04_crate-map.md` and this guide;
- current owners in `magicblock-chainlink`, `magicblock-account-cloner`, and `magicblock-api`.

Risks:

- deleting exported symbols can be a public API break even if no current workspace consumer uses them;
- account availability behavior belongs outside this crate in the current architecture.

## Tests and validation

For documentation-only changes:

```bash
git diff --check -- agents/crates/magicblock-accounts.md agents/04_crate-map.md AGENTS.md
```

Also verify:

- `agents/crates/magicblock-accounts.md` exists;
- `agents/04_crate-map.md` points future agents to this guide;
- `AGENTS.md` mentions the new crate guide if the example crate-guide list is kept current;
- no files under `prompts/**` are staged or committed.

For Rust changes in this crate, run targeted checks first:

```bash
cargo fmt
cargo clippy -p magicblock-accounts --all-targets -- -D warnings
cargo nextest run -p magicblock-accounts
```

For changes that affect scheduled commits, pending recovery, or undelegation, also run relevant adjacent checks when practical:

```bash
cargo nextest run -p magicblock-api
cargo nextest run -p magicblock-program process_schedule_commit
cargo nextest run -p magicblock-committor-service
```

Integration suites for end-to-end behavior:

```bash
cd test-integration
make test-schedule-intents
make test-committor
make test-task-scheduler
```

Use narrower committor targets from `agents/05_testing-and-validation.md` when full committor coverage is too slow but the change is localized.

Broader baseline validation remains the repository standard from `agents/05_testing-and-validation.md`:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance validation expectations:

- Documentation-only changes have no runtime performance impact.
- Changes to slot-ticker commit processing, intent metadata handling, Chainlink undelegation notification, or result handling should report whether they add work to periodic slot processing, committor scheduling, or transaction-scheduler submission.
- If a change can increase commit latency or result-processing lag, validate with schedule-intent/committor tests or a targeted measurement and report any unmeasured risk.

## Related docs

- `AGENTS.md` — required agent workflow and documentation-memory rules.
- `agents/00_overview.md` — validator runtime model and core concepts.
- `agents/02_specification.md` — commit, undelegation, committor service, and recovery behavior.
- `agents/03_architecture.md` — account synchronization and base-layer settlement boundaries.
- `agents/04_crate-map.md` — crate ownership map and pointer back to this guide.
- `agents/05_testing-and-validation.md` — repository validation commands and integration suite names.
- `agents/06_agent-memory-and-docs.md` — rules for keeping agent documentation current.
- `agents/crates/magicblock-account-cloner.md` — current account-cloner guide; useful because this crate aliases the production cloner stack through Chainlink.
- `agents/crates/magicblock-chainlink.md` — Chainlink lifecycle/delegation guide, especially `undelegation_requested` and delegation checks.
- `magicblock-accounts/README.md` — historical summary; verify against source before using.
- `magicblock-api/src/tickers.rs` — slot ticker that invokes this crate.
- `magicblock-api/src/magic_validator.rs` — startup/shutdown and recovery wiring.
- `programs/magicblock/src/schedule_transactions/` — Magic Program scheduling and acceptance processors.
- `magicblock-committor-service/` — base-layer intent execution, persistence, and recovery source.
