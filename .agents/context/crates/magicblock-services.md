# `magicblock-services`

## Purpose

`magicblock-services` contains small reusable service adapters that run alongside the validator and communicate through existing validator/RPC contracts. Its current responsibility is the action-callback adapter used by the committor pipeline to notify user callback programs about Magic Action results.

High-level responsibilities:

- implement `magicblock_core::traits::ActionsCallbackScheduler` for validator-runtime use;
- turn `BaseActionCallback` payloads from committed Magic Actions into local callback transactions;
- wrap callback results in the `magicblock-magic-program-api` `MagicResponse::V1` wire shape;
- submit callback transactions asynchronously through Solana's nonblocking `RpcClient`.

This crate is settlement-adjacent and can affect Magic Action user experience. It is not the committor itself, does not own intent execution or persistence, and must stay generic enough to be used as a service adapter rather than a protocol owner.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-services` change. In particular, update it for changes to:

- exported modules or public constructors in `magicblock-services/src/lib.rs` or `actions_callback_service.rs`;
- the `ActionsCallbackService` transaction layout, signer handling, blockhash source, or callback response encoding;
- how callback signatures, errors, receipts, discriminators, payloads, or account metas are propagated;
- asynchronous send behavior, logging, retry/confirmation semantics, or Tokio runtime assumptions;
- call-site wiring in `magicblock-api` or committor expectations around `ActionsCallbackScheduler`;
- validation commands or integration suites relevant to action callbacks.

Because callbacks are a cross-crate contract between Magic Program scheduling, committor execution, and user callback programs, also update this file when another crate changes `BaseActionCallback`, `MagicResponse`, `CallbackInstruction`, or `ActionsCallbackScheduler` semantics.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-services/Cargo.toml` | Package metadata and dependencies. Depends on `magicblock-core`, `magicblock-magic-program-api`, Solana transaction/RPC crates, Tokio, and tracing. |
| `magicblock-services/src/lib.rs` | Public module surface. Currently exports only `actions_callback_service`. |
| `magicblock-services/src/actions_callback_service.rs` | Implements `ActionsCallbackService<L>` and `ActionsCallbackScheduler`. Builds and sends callback transactions. |
| `magicblock-api/src/magic_validator.rs` | Runtime wiring. `init_committor_service` creates `ActionsCallbackService` with the validator keypair, local latest-block provider, and an RPC client pointed at the validator aperture HTTP endpoint. |
| `magicblock-core/src/traits.rs` | Defines `ActionsCallbackScheduler`, `ActionResult`, `ActionError`, and `CallbackScheduleError`. |
| `magicblock-core/src/intent.rs` | Defines `BaseActionCallback`, the callback payload this crate consumes. |
| `magicblock-committor-service/src/intent_executor/utils.rs` | Extracts callbacks from action tasks and invokes the scheduler after success, action failure, or timeout. |
| `programs/magicblock/src/schedule_transactions/process_add_action_callback.rs` | Magic Program path that attaches callbacks to scheduled base actions. |

Main consumers:

- `magicblock-api`, which constructs the concrete callback service for the validator;
- `magicblock-committor-service`, which is generic over `ActionsCallbackScheduler` and calls it from intent execution;
- callback-capable Magic Action flows scheduled through `programs/magicblock`.

There are no crate-local tests or README files for `magicblock-services` at the time of writing. Use the committor integration tests when callback behavior changes.

## Public API shape / Main public types and APIs

### Module surface

`src/lib.rs` exposes:

- `pub mod actions_callback_service`.

Keep this surface small. New shared services should be added only when they are truly generic validator service adapters and not better owned by API orchestration, committor, RPC-client, or Magic Program crates.

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

## Runtime flows

### Validator startup wiring

1. `magicblock-api::MagicValidator::init_committor_service` creates a Solana `RpcClient` pointed at `config.aperture.listen.http()`.
2. It constructs `ActionsCallbackService::new(...)` with the validator keypair and `LatestBlock` handle.
3. The service is passed into `CommittorService::try_start(...)` as the concrete `ActionsCallbackScheduler`.
4. The committor keeps using the trait boundary; it does not depend directly on this crate's concrete type.

Preserve this separation. `magicblock-services` should not reach back into validator orchestration or committor internals.

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

## Important invariants

1. `schedule` must return exactly one `Result<Signature, CallbackScheduleError>` for each input callback, preserving input order.
2. Callback response data must preserve the `discriminator || bincode(MagicResponse::V1)` layout unless a coordinated wire-format migration is implemented.
3. The validator authority must remain the outer transaction payer and signer unless startup/wallet semantics are intentionally changed.
4. `CALLBACK_SIGNER` may be a signer only inside the inner instruction; it must be non-signer in the outer transaction account metas.
5. Inner account meta writability must be propagated from `BaseActionCallback`; this crate should not reinterpret callback account authorization.
6. The `Noop(counter)` uniqueness instruction must keep otherwise duplicate callback transactions from producing identical signatures.
7. Do not add blocking RPC confirmation or retry loops to the committor hot path without an explicit architecture decision and performance review.
8. Keep the crate dependency-light. Generic service adapters should not pull in validator orchestration, persistence, or large protocol owners.

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
- `magicblock-config` endpoint settings used by `config.aperture.listen.http()` and `config.rpc_url()`;
- `magicblock-committor-service::CommittorService::try_start` generic bounds.

Be explicit about whether callbacks should be sent to local aperture or base-layer RPC.

### Adding another shared service adapter

Start with `magicblock-services/src/lib.rs` and ask whether the new adapter belongs here. Prefer this crate only for small generic services that implement shared traits. Put orchestration in `magicblock-api`, settlement logic in `magicblock-committor-service`, RPC policy in `magicblock-rpc-client`, and protocol wire types in `magicblock-magic-program-api`.

## Tests and validation

For documentation-only changes touching this guide:

```bash
git diff -- .agents/context/crates/magicblock-services.md .agents/context/crate-map.md AGENTS.md
```

For Rust changes in `magicblock-services`, run at minimum:

```bash
cargo fmt
cargo nextest run -p magicblock-services
```

Because this crate has no crate-local tests, callback behavior changes should also run the relevant committor tests, especially suites that exercise action callbacks and timeouts:

```bash
cargo nextest run -p magicblock-committor-service
cd test-integration && make test-committor
```

Before handoff, run or justify skipping the broader baseline from `.agents/rules/testing-and-validation.md`:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance-risk reporting: callback scheduling is settlement-adjacent and currently fire-and-forget. If a change adds synchronous RPC work, retries, confirmation, persistence, extra serialization, or unbounded spawning/logging, report the expected impact on committor throughput and callback latency.

## Related docs

- `AGENTS.md` for required agent guidance and documentation stewardship rules.
- `.agents/specs/validator-specification.md` for Magic Actions, commit/undelegation, committor, and callback-related protocol context.
- `.agents/context/architecture.md` for service boundaries and base-layer settlement architecture.
- `.agents/context/crate-map.md` for workspace crate ownership and consumers.
- `.agents/rules/testing-and-validation.md` for baseline validation commands.
- `.agents/context/crates/magicblock-core.md` for `ActionsCallbackScheduler`, `BaseActionCallback`, and related shared trait/type contracts.
- `.agents/context/crates/magicblock-api.md` for validator startup and service wiring.
- `.agents/context/crates/magicblock-magic-program-api.md` for callback instruction and response wire types.
- `.agents/context/crates/magicblock-rpc-client.md` if callback delivery begins using shared send/confirm helpers.
