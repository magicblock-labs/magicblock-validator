# `magicblock-services`

## Purpose

`magicblock-services` contains small reusable services and adapters that run alongside the validator and communicate through existing validator/RPC contracts. It currently owns the action-callback adapter used by the committor pipeline and the owner-program undelegation request service that turns observed Delegation Program requests into local `ScheduleCommitAndUndelegate` transactions.

High-level responsibilities:

- implement `magicblock_core::traits::ActionsCallbackScheduler` for validator-runtime use;
- turn `BaseActionCallback` payloads from committed Magic Actions into local callback transactions;
- wrap callback results in the `magicblock-magic-program-api` `MagicResponse::V1` wire shape;
- submit callback transactions asynchronously through Solana's nonblocking `RpcClient`;
- subscribe to Chainlink-observed Delegation Program `UndelegationRequest` accounts;
- validate request PDAs and delegated-account state before scheduling local validator-signed `ScheduleCommitAndUndelegate` transactions.

This crate is settlement-adjacent and can affect Magic Action user experience and undelegation liveness. It is not the committor itself, does not own base-layer intent execution or persistence, and should stay limited to service/adaptor boundaries rather than becoming a protocol owner.

End-to-end commit/undelegation semantics live in .agents/specs/validator-specification.md; this crate owns callback transaction construction/scheduling and request-observer submission of local schedule transactions.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-services` change. In particular, update it for changes to:

- exported modules or public constructors in `magicblock-services/src/lib.rs`, `actions_callback_service.rs`, or `undelegation_request_service.rs`;
- the `ActionsCallbackService` transaction layout, signer handling, blockhash source, or callback response encoding;
- the `UndelegationRequestService` request validation, retry policy, Chainlink checks, transaction layout, signer handling, blockhash source, or subscription/polling behavior;
- how callback signatures, errors, receipts, discriminators, payloads, or account metas are propagated;
- asynchronous send behavior, logging, retry/confirmation semantics, or Tokio runtime assumptions;
- call-site wiring in `magicblock-api`, committor expectations around `ActionsCallbackScheduler`, or Chainlink observer contracts consumed by the undelegation request service;
- validation commands or integration suites relevant to action callbacks or owner-program undelegation requests.

Because callbacks are a cross-crate contract between Magic Program scheduling, committor execution, and user callback programs, also update this file when another crate changes `BaseActionCallback`, `MagicResponse`, `CallbackInstruction`, or `ActionsCallbackScheduler` semantics.

For the general documentation-update rule, see .agents/memory/agent-memory-and-docs.md.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-services/Cargo.toml` | Package metadata and dependencies. Depends on shared core/Magic Program types, Chainlink/cloner wiring, Solana transaction/RPC crates, Tokio, and tracing. |
| `magicblock-services/src/lib.rs` | Public module surface. Exports `actions_callback_service` and `undelegation_request_service`. |
| `magicblock-services/src/actions_callback_service.rs` | Implements `ActionsCallbackService<L>` and `ActionsCallbackScheduler`. Builds and sends callback transactions. |
| `magicblock-services/src/undelegation_request_service.rs` | Implements `UndelegationRequestService`. Consumes observed DLP request accounts and schedules local `ScheduleCommitAndUndelegate` transactions. |
| `magicblock-api/src/magic_validator.rs` | Runtime wiring. `init_committor_processor` creates `ActionsCallbackService`; validator startup creates and starts `UndelegationRequestService` for non-replica validators. |
| `magicblock-core/src/traits.rs` | Defines `ActionsCallbackScheduler`, `ActionResult`, `ActionError`, and `CallbackScheduleError`. |
| `magicblock-core/src/intent.rs` | Defines `BaseActionCallback`, the callback payload this crate consumes. |
| `magicblock-chainlink` | Supplies `subscribe_undelegation_requests`, delegation-state checks, and `undelegation_requested` lifecycle notification. |
| `magicblock-committor-service/src/intent_executor/utils.rs` | Extracts callbacks from action tasks and invokes the scheduler after success, action failure, or timeout. |
| `programs/magicblock/src/schedule_transactions/process_add_action_callback.rs` | Magic Program path that attaches callbacks to scheduled base actions. |
| `programs/magicblock/src/schedule_transactions/process_schedule_commit.rs` | Magic Program path reached by the local `ScheduleCommitAndUndelegate` transaction. |

Main consumers:

- `magicblock-api`, which constructs the concrete callback service and undelegation request service for the validator;
- `magicblock-committor-service`, which is generic over `ActionsCallbackScheduler` and calls it from intent execution;
- Chainlink, which publishes observed undelegation request updates consumed by this crate;
- callback-capable Magic Action flows scheduled through `programs/magicblock`.

There are no crate-local tests or README files for `magicblock-services` at the time of writing. Use the committor integration tests when callback behavior changes.

## Public API shape / Main public types and APIs

### Module surface

`src/lib.rs` exposes:

- `pub mod actions_callback_service`.
- `pub mod undelegation_request_service`.

Keep this surface small. New shared services should be added only when they are truly validator background services/adapters and not better owned by API orchestration, committor, RPC-client, Chainlink, or Magic Program crates.

### `ActionsCallbackService<L>`

`ActionsCallbackService<L>` stores:

- `Arc<solana_rpc_client::nonblocking::rpc_client::RpcClient>` for callback transaction submission;
- a validator `Keypair` authority used as fee payer and transaction signer;
- `latest_block: L`, where `L: LatestBlockProvider`, used to obtain the recent blockhash for each callback transaction.

Public constructor:

```rust
ActionsCallbackService::new(rpc_client, authority, latest_block)
```

The service implements `Clone` when `L: Clone`. Cloning uses `authority.insecure_clone()` because the concrete service must satisfy `ActionsCallbackScheduler: Clone + Send + Sync + 'static`. Do not accidentally replace this with shared mutable keypair state or a non-`Clone` service without updating all committor generic bounds and runtime wiring.

### `ActionsCallbackScheduler` implementation

`schedule(callbacks, signature, result)` returns one result per input callback:

- construction/signing failures become `Err(CallbackScheduleError)` at the matching position;
- successfully built transactions return their precomputed callback transaction signature immediately;
- valid transactions are sent later in a spawned Tokio task.

The return value reports scheduling/build success, not confirmed on-chain callback execution. The spawned task logs send failures but does not retry or update the returned result after the fact.

### `UndelegationRequestService`

`UndelegationRequestService` stores:

- `Arc<ProdChainlink<ChainlinkCloner>>` for observed request subscription, delegation checks, and undelegation tracking notification;
- `TransactionSchedulerHandle` for submitting local validator transactions;
- validator `Keypair` authority used as payer and signer;
- a `LatestBlockProvider` wrapper used to sign each local transaction with a fresh ER blockhash;
- a `CancellationToken` used by `stop`.

Public API:

```rust
UndelegationRequestService::new(chainlink, scheduler, authority, latest_block)
service.start()
service.stop()
```

`start` subscribes to Chainlink observed undelegation requests and spawns one background task. The service does not own request discovery itself; Chainlink owns base-layer subscription/scanning and emits `ObservedUndelegationRequest` values.

For each observed request, the service:

1. verifies the request PDA matches the delegated account;
2. asks Chainlink to materialize the delegated account if it is missing locally;
3. checks the delegated account is delegated on base and ER;
4. best-effort notifies Chainlink with `undelegation_requested`;
5. submits a local validator-signed `ScheduleCommitAndUndelegate` transaction with the validator as payer/signer, `MAGIC_CONTEXT_PUBKEY`, and each delegated account as writable non-signer.

Transient delegation-check and local-scheduling failures are retried three times with short exponential backoff. Invalid request PDAs and non-delegated accounts are skipped.

## Runtime flows

### Validator startup wiring

1. `magicblock-api::MagicValidator::init_committor_service` creates a Solana `RpcClient` pointed at `config.aperture.listen.http()`.
2. It constructs `ActionsCallbackService::new(...)` with the validator keypair and `LatestBlock` handle.
3. The service is passed into `CommittorService::try_start(...)` as the concrete `ActionsCallbackScheduler`.
4. The committor keeps using the trait boundary; it does not depend directly on this crate's concrete type.

Preserve this separation. `magicblock-services` should not reach back into validator orchestration or committor internals.

### Undelegation Request Service Startup

```text
magicblock-api::MagicValidator::try_from_config
  -> create UndelegationRequestService for non-replica validators
magicblock-api::MagicValidator::start
  -> service.start()
  -> Chainlink observed request subscription
  -> local ScheduleCommitAndUndelegate transaction
  -> committor scheduled-intent service handles the resulting Magic Program intent
```

Replica validators do not start this service because replica Chainlink is disabled and ownership/lifecycle decisions should come from primary state replication.

### Callback scheduling and transaction construction

```text
committor intent executor
  -> ActionsCallbackScheduler::schedule(callbacks, base_action_signature, result)
  -> build callback transactions with latest local blockhash
  -> return callback transaction signatures/errors
  -> tokio::spawn sends valid transactions via RpcClient
```

For each callback:

1. `build_transactions` reads `latest_block.blockhash()` once for the batch.
2. It converts `ActionResult` into `Result<(), String>` so the callback program receives a serializable success/error response.
3. It adds a Magic Program `Noop(counter)` instruction before the callback instruction. The static `AtomicU64` counter makes otherwise identical callback transactions unique.
4. It builds the outer `CallbackInstruction::ExecuteCallback` instruction for `CALLBACK_PROGRAM_ID`.
5. It signs a legacy `VersionedTransaction` with the validator authority as payer.

The current implementation uses `Ordering::Relaxed` for uniqueness only; no ordering or synchronization semantics are implied.

### Inner callback instruction encoding

`build_inner_instruction` creates the user-program instruction that the callback program will invoke:

1. It wraps the outcome in `MagicResponse::V1(MagicResponseV1 { ok, data, error, receipt })`.
2. `data` starts with `callback.discriminator` and appends `bincode::serialize(&response)`.
3. `receipt` is present only when the committor supplied a base-action transaction signature.
4. Account metas are copied from `callback.account_metas_per_program`.
5. Only `CALLBACK_SIGNER` is marked as signer in the inner instruction; all other metas preserve writability but are not made signers here.

### Outer callback instruction accounts

`build_callback_instruction` wraps the inner instruction for the callback program. The outer accounts are ordered as:

1. validator authority, readonly signer;
2. `CALLBACK_SIGNER`, readonly non-signer PDA;
3. destination program ID, readonly non-signer;
4. all inner instruction accounts, with `is_signer` forcibly set to `false` because a PDA cannot sign the outer transaction directly.

Do not reorder these accounts without checking `magicblock-magic-program-api` and the callback program processor that consumes `CallbackInstruction::ExecuteCallback`.

## Important internals and caveats

### Fire-and-forget send semantics

`schedule` uses `tokio::spawn` and `join_all` over `rpc_client.send_transaction(tx)` for all valid callback transactions. This requires a live Tokio runtime at the call site. The validator and committor run inside Tokio today; if a future caller invokes the scheduler outside a runtime, scheduling will panic.

Callback sends are not confirmed and are not retried. This keeps the committor callback handoff lightweight, but it means callback delivery is best-effort after transaction construction. If stronger delivery is required, that is a product/architecture change touching committor reporting, persistence, and possibly `magicblock-rpc-client`; do not silently add blocking confirmation in this crate.

### Local RPC target

`magicblock-api` currently points this service at the validator's own aperture HTTP endpoint, not directly at the base-layer RPC URL used by the committor. This preserves the local callback execution path. Changing the endpoint changes where callback transactions are executed and must be reviewed as a protocol/architecture change.

### Wire compatibility

The callback instruction data combines a program-specific discriminator with a bincode-encoded `MagicResponse`. This is a user-program-facing wire contract. Changes to response versioning, serialization, or account meta treatment must be coordinated with `magicblock-magic-program-api`, Magic Program validation, and downstream callback program expectations.

### Error reporting boundary

`CallbackScheduleError` only covers local serialization/signing failures. RPC send failures are logged asynchronously with the callback transaction signature and do not flow back into `IntentExecutionReport` after `schedule` returns.

### Undelegation request observer boundary

The undelegation request service is a trigger: it only schedules the local Magic Program intent. It does not build or send base-layer commit/undelegate transactions, persist intent rows, or decide committor task strategy. After local scheduling, the normal committor intent service accepts and executes the resulting scheduled intent.

The service validates the request PDA from delegated account before scheduling. It must not trust request-local owner, rent payer, or commit nonce fields; current request data carries the delegated account and expiry slot only.

Before checking base/ER delegation readiness, the service calls Chainlink `ensure_accounts` for the delegated account. This lets polling recover valid requests for accounts that are delegated on base but not yet present in the ER bank, instead of skipping the request until unrelated traffic clones the account.

The service logs expired requests but still schedules normal undelegation when the delegated account is still valid. This preserves the best chance of committing ER state and clearing lifecycle state instead of leaving timeout/rollback handling to a stale request.

## Important invariants

1. `schedule` must return exactly one `Result<Signature, CallbackScheduleError>` for each input callback, preserving input order.
2. Callback response data must preserve the `discriminator || bincode(MagicResponse::V1)` layout unless a coordinated wire-format migration is implemented.
3. The validator authority must remain the outer transaction payer and signer unless startup/wallet semantics are intentionally changed.
4. `CALLBACK_SIGNER` may be a signer only inside the inner instruction; it must be non-signer in the outer transaction account metas.
5. Inner account meta writability must be propagated from `BaseActionCallback`; this crate should not reinterpret callback account authorization.
6. The `Noop(counter)` uniqueness instruction must keep otherwise duplicate callback transactions from producing identical signatures.
7. Do not add blocking RPC confirmation or retry loops to the committor hot path without an explicit architecture decision and performance review.
8. Keep service dependencies scoped to the adapter. Do not pull validator orchestration or persistence into this crate.
9. `UndelegationRequestService` must verify the request PDA before scheduling.
10. `UndelegationRequestService` must schedule only when the delegated account is delegated on both base and ER.
11. `UndelegationRequestService` must use the validator authority as local transaction payer and signer.
12. Request observation must remain non-replica-only unless replica lifecycle semantics are redesigned.
13. The request service must not bypass the Magic Program scheduled-intent path or call the committor directly for owner-program requests.
14. Missing Chainlink `undelegation_requested` notification should be logged but must not prevent local scheduling under the current best-effort policy.

## Common change areas and what to inspect

### Changing callback transaction shape

Start with:

- `magicblock-services/src/actions_callback_service.rs`;
- `magicblock-magic-program-api` callback instruction and response types;
- callback program processing for `CALLBACK_PROGRAM_ID`;
- `programs/magicblock/src/schedule_transactions/process_add_action_callback.rs`.

Check account order, signer flags, discriminator/response encoding, and whether user callback programs require compatibility migration.

### Changing delivery guarantees

Start with:

- `ActionsCallbackService::schedule`;
- `magicblock-committor-service/src/intent_executor/utils.rs::handle_actions_result`;
- `IntentExecutionReport` callback report handling;
- `magicblock-rpc-client` send/confirm APIs if confirmation is needed.

Do not make `schedule` block on network confirmation unless the committor timeout, persistence, and report semantics are updated intentionally.

### Changing validator wiring

Start with:

- `magicblock-api/src/magic_validator.rs::init_committor_service`;
- `magicblock-api/src/magic_validator.rs` construction/start/stop of `UndelegationRequestService`;
- `magicblock-config` endpoint settings used by `config.aperture.listen.http()` and `config.rpc_url()`;
- `magicblock-committor-service::CommittorService::try_start` generic bounds.

Be explicit about whether callbacks should be sent to local aperture or base-layer RPC.

### Changing owner-program undelegation request handling

Start with:

- `magicblock-services/src/undelegation_request_service.rs`;
- `magicblock-chainlink` observed undelegation request subscription and delegation checks;
- `programs/magicblock/src/schedule_transactions/process_schedule_commit.rs`;
- `magicblock-committor-service/src/service.rs` and task building for accepted commit-and-undelegate intents;
- DLP request PDA and metadata semantics in `magicblock-delegation-program-api`.

Check PDA validation, replica gating, retry behavior, local transaction accounts, blockhash freshness, and whether failures should be skipped, retried, or surfaced.

### Adding another shared service adapter

Start with `magicblock-services/src/lib.rs` and ask whether the new adapter belongs here. Prefer this crate only for small generic services that implement shared traits. Put orchestration in `magicblock-api`, settlement logic in `magicblock-committor-service`, RPC policy in `magicblock-rpc-client`, and protocol wire types in `magicblock-magic-program-api`.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-services`.
- Consumer validation intent: because this crate has limited crate-local coverage, include relevant `magicblock-committor-service` checks for callback behavior changes and request-driven undelegation integration checks for undelegation request changes.
- Relevant integration suites: committor suites that exercise action callbacks/timeouts, plus schedule-commit or undelegation request suites when `UndelegationRequestService` changes; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Performance-risk validation intent: report any added synchronous RPC work, retries, confirmation, persistence, extra serialization, or unbounded spawning/logging and its effect on committor throughput and callback latency.


## Adjacent implementation references

- `.agents/context/crates/magicblock-core.md` — `ActionsCallbackScheduler`, `BaseActionCallback`, and related shared trait/type contracts.
- `.agents/context/crates/magicblock-api.md` — validator startup and service wiring.
- `.agents/context/crates/magicblock-chainlink.md` — observed undelegation request subscription and delegation lifecycle checks.
- `.agents/context/crates/magicblock-committor-service.md` — scheduled intent acceptance/execution after request service submits the local schedule transaction.
- `.agents/context/crates/magicblock-magic-program-api.md` — callback instruction and response wire types.
- `.agents/context/crates/magicblock-rpc-client.md` — relevant if callback delivery begins using shared send/confirm helpers.
- `magicblock-committor-service/src/intent_executor/utils.rs` — committor callback scheduling call site.
- `programs/magicblock/src/schedule_transactions/process_add_action_callback.rs` — Magic Program callback attachment path.
- `programs/magicblock/src/schedule_transactions/process_schedule_commit.rs` — Magic Program schedule path used by owner-program undelegation requests.
