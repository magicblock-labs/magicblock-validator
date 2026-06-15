# `magicblock-magic-program-api`

## Purpose

`magicblock-magic-program-api` is the shared wire-contract crate for the Magic Program and its built-in companion programs. It defines the program IDs, fixed accounts, PDA helpers, instruction enums, scheduling argument structs, clone metadata, task request payloads, callback response payloads, and Solana-type compatibility exports used by the validator and test programs.

This crate is small, but it is protocol- and compatibility-sensitive. Its types are serialized with `bincode` into transactions, persisted scheduled intents, executor TLS payloads, and callback instructions. Changes can affect Magic Program CPI callers, account cloning, task scheduling, callback delivery, committor intent construction, account reset/blacklist behavior, and integration tests. Treat enum variant order, field order, constants, and feature-gated public type aliases as wire/API contracts.

High-level responsibilities:

- expose the Magic Program ID (`Magic111...`) plus fixed companion IDs/accounts such as `MAGIC_CONTEXT_PUBKEY`, `EPHEMERAL_VAULT_PUBKEY`, `CRANK_PROGRAM_ID`, `CALLBACK_PROGRAM_ID`, and `POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID`;
- define `MagicBlockInstruction`, `CallbackInstruction`, and `PostDelegationActionExecutorInstruction` payloads consumed by `programs/magicblock` and validator-built internal transactions;
- define commit/action/undelegation bundle argument types used by application programs and Magic Program scheduling processors;
- define task scheduling/cancel request payloads sent from Magic Program execution into `magicblock-core` TLS and the task scheduler;
- define callback response types (`MagicResponse`, `MagicResponseV1`, `ActionReceipt`) delivered to base-layer callback programs;
- provide a `compat` boundary so public API users can opt into Solana 2.x-compatible types through the `backward-compat` feature while the default uses workspace Solana 3.x types.

Do not put Magic Program execution logic, validation policy, persistence, RPC behavior, or committor delivery logic in this crate. Those belong in `programs/magicblock`, `magicblock-core`, `magicblock-accounts`, and the committor/service crates.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-magic-program-api` change. In particular, update it for changes to:

- any public constant, program ID, fixed account, PDA seed, PDA derivation, or rent/context-size constant;
- `MagicBlockInstruction`, `CallbackInstruction`, `PostDelegationActionExecutorInstruction`, `AccountCloneFields`, account-modification types, or serialization behavior;
- commit, action, undelegation, intent-bundle, callback, task, or `ShortAccountMeta` argument structs/enums;
- the `backward-compat` feature or `compat` module public type aliases;
- callback response payload shape or receipt semantics;
- account metas expected by Magic Program processors or helper builders in `programs/magicblock/src/utils/instruction_utils.rs`;
- validation commands or integration suites that should be run after API changes.

Because this crate defines shared wire formats, also update this document when another crate changes how these API types are interpreted.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-magic-program-api/Cargo.toml` | Package metadata, minimal dependencies, and `backward-compat` feature wiring for Solana 2.x-compatible public types. |
| `magicblock-magic-program-api/src/lib.rs` | Public module surface, `declare_id!`, fixed program/account IDs, `MAGIC_CONTEXT_SIZE`, and `EPHEMERAL_RENT_PER_BYTE`. |
| `magicblock-magic-program-api/src/compat.rs` | Public Solana type boundary. Default exports workspace Solana 3.x `Pubkey`, `Instruction`, `AccountMeta`, `AccountInfo`, `Signature`; `backward-compat` exports corresponding 2.x-compatible crates. |
| `magicblock-magic-program-api/src/instruction.rs` | Magic Program and companion built-in instruction enums plus account modification and clone metadata payloads. This is the most wire-sensitive file. |
| `magicblock-magic-program-api/src/args.rs` | Serializable argument structs/enums for base actions, commits, undelegation, intent bundles, action callbacks, and scheduled tasks. |
| `magicblock-magic-program-api/src/pda.rs` | PDA seeds/helpers for crank and callback execution signers. `CALLBACK_SIGNER` and bump are compile-time derived. |
| `magicblock-magic-program-api/src/response.rs` | Serializable callback response payloads sent by `magicblock-services` and decoded by callback programs. |
| `programs/magicblock/src/magicblock_processor.rs` | Deserializes and dispatches `MagicBlockInstruction` and `CallbackInstruction` values. Keep this aligned with instruction variants. |
| `programs/magicblock/src/utils/instruction_utils.rs` | Validator/program helper builders for many `MagicBlockInstruction` variants and account metas. |
| `programs/magicblock/src/magic_scheduled_base_intent.rs` | Interprets `MagicIntentBundleArgs`, commit/action args, `ShortAccountMeta`, fees, duplicate checks, and secure/legacy action behavior. |
| `magicblock-services/src/actions_callback_service.rs` | Builds `CallbackInstruction::ExecuteCallback` and bincode-encoded `MagicResponse` values. |
| `magicblock-account-cloner/src/lib.rs` | Builds clone, cleanup, post-delegation action, program-finalize, and task-related Magic Program instructions using this API. |
| `magicblock-processor/tests/ephemeral_accounts.rs` and `magicblock-processor/tests/post_delegation_actions.rs` | Focused processor coverage for API-driven ephemeral account and post-delegation-action behavior. |
| `test-integration/schedulecommit/` and `test-integration/test-schedule-intent/` | Integration coverage for commit scheduling, intent bundles, commit limits, security, undelegation, actions, and callbacks. |

Main consumers include:

- `programs/magicblock`, the runtime implementation that deserializes and executes these instructions;
- `magicblock-core`, which stores task requests and callback/action payload dependencies;
- `magicblock-account-cloner`, `magicblock-chainlink`, `magicblock-accounts-db`, `magicblock-api`, `magicblock-processor`, and `magicblock-services`;
- workspace/test programs such as `programs/guinea`, `test-integration/programs/schedulecommit`, and `test-integration/programs/flexi-counter`;
- integration suites under `test-integration/schedulecommit` and `test-integration/test-schedule-intent`.

## Public API shape / Main public types and APIs

### Root exports and constants

`src/lib.rs` publicly exports `args`, `compat`, `instruction`, `pda`, and `response`, then re-exports `compat::{declare_id, pubkey, Pubkey}`. The crate declares the Magic Program ID and fixed companion accounts/programs:

- `ID` / `id()` from `declare_id!("Magic11111111111111111111111111111111111111")`;
- `CRANK_PROGRAM_ID` for task/crank execution;
- `CALLBACK_PROGRAM_ID` for callback executor built-in instructions;
- `POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID` for same-transaction post-delegation action execution after cloning;
- `MAGIC_CONTEXT_PUBKEY`, the fixed account that stores scheduled intents;
- `EPHEMERAL_VAULT_PUBKEY`, the fixed vault account for ephemeral-account rent transfers;
- `MAGIC_CONTEXT_SIZE = 5 MiB`;
- `EPHEMERAL_RENT_PER_BYTE = 32` lamports/byte.

These values are referenced by startup funding/reset logic, chainlink clone blacklists, Magic Program processors, tests, and account builders. Renaming or deriving them differently is a protocol change.

### Compatibility boundary

`compat.rs` exports public Solana types. With default features it uses workspace Solana 3.x crates. With `--features backward-compat`, it uses optional `solana-program`/`solana-signature` compatibility dependencies in the `>=2.0, <3` range.

Use `crate::compat::{Instruction, AccountMeta, AccountInfo, Signature}` and `crate::Pubkey` in public API structs. Do not mix raw Solana versions in public fields unless the compatibility contract is intentionally changed.

### Instruction enums

`instruction.rs` defines:

- `MagicBlockInstruction`: the primary bincode-serialized instruction enum for the Magic Program. Variants cover account modification, legacy and bundled commit scheduling, task scheduling/canceling, executable-check toggles, no-op uniqueness, ephemeral account create/resize/close, clone/chunk/cleanup, program finalization, callback attachment, account eviction, and crank execution.
- `CallbackInstruction`: instruction enum for the callback executor built-in. Current variant is `ExecuteCallback { instruction }`.
- `PostDelegationActionExecutorInstruction`: instruction enum for the post-delegation action executor built-in. Current variant is `Execute { cloned_account_pubkey, actions }`.
- `AccountCloneFields`: clone metadata (`lamports`, `owner`, `executable`, `delegated`, `confined`, `remote_slot`) that must preserve remote/local account semantics across clone instructions.
- `AccountModification` and `AccountModificationForInstruction`: validator-only account flag/owner modifications.

`MagicBlockInstruction::try_to_vec()` is a thin `bincode::serialize` helper. Most call sites use `Instruction::new_with_bincode` directly.

### Commit/action/intent args

`args.rs` defines the public data model for base-layer intent scheduling:

- `ActionArgs` carries action data plus an optional `escrow_index` sentinel defaulting to `255`.
- `BaseActionArgs` identifies the destination program, compact account metas, compute units, escrow authority index, and embedded action args.
- `CommitTypeArgs::{Standalone, WithBaseActions}` lists committed-account indices and optional post-commit base actions.
- `UndelegateTypeArgs::{Standalone, WithBaseActions}` lists undelegation behavior and optional base actions.
- `CommitAndUndelegateArgs` combines commit and undelegate parts.
- `MagicBaseIntentArgs` is the legacy single-intent shape used by `ScheduleBaseIntent`.
- `MagicIntentBundleArgs` is the recommended bundle shape used by `ScheduleIntentBundle`; it can include optional commit, commit-and-undelegate, commit-finalize, commit-finalize-and-undelegate, and standalone base actions.
- `ShortAccountMeta` carries only `pubkey` and `is_writable`; it intentionally has no caller-controlled `is_signer` flag.
- `AddActionCallbackArgs` attaches callback metadata to the latest scheduled action in the current slot/blockhash.

All account references inside commit/action args are compact indices into the instruction account list. The Magic Program resolves them in `programs/magicblock/src/magic_scheduled_base_intent.rs`.

### Task args

Task scheduling uses:

- `ScheduleTaskArgs` in `MagicBlockInstruction::ScheduleTask`;
- `TaskRequest::{Schedule, Cancel}` plus `ScheduleTaskRequest` and `CancelTaskRequest` to communicate side effects through `magicblock-core::tls::ExecutionTlsStash` to `magicblock-task-scheduler`;
- `TaskRequest::id()` to abstract over schedule/cancel IDs.

### PDA helpers

`pda.rs` defines:

- `CRANK_SEED = b"crank-executor"` and `crank_signer_pda(authority)`, derived under `CRANK_PROGRAM_ID`;
- `CALLBACK_SEED = b"callback-executor"`;
- `CALLBACK_SIGNER` and `CALLBACK_SIGNER_BUMP`, compile-time derived under `CALLBACK_PROGRAM_ID` with no authority seed.

Callback account metas treat `CALLBACK_SIGNER` specially: user-facing `ShortAccountMeta` cannot set signer bits, but callback builders mark this PDA as signer for inner callback instructions.

### Callback responses

`response.rs` defines:

- `MagicResponse::V1(MagicResponseV1)` with convenience accessors `ok()`, `data()`, and `error()`;
- `MagicResponseV1 { ok, data, error, receipt }`, bincode-encoded after a callback-specific discriminator by `magicblock-services`;
- `ActionReceipt { signature }`, present when the base action transaction signature is available.

Callback programs, such as `test-integration/programs/flexi-counter`, deserialize this payload directly.

## Runtime flows

### Magic Program instruction dispatch

```text
caller / validator helper
  -> Instruction::new_with_bincode(program_id, &MagicBlockInstruction::..., metas)
  -> programs/magicblock/src/magicblock_processor.rs
  -> bincode::deserialize::<MagicBlockInstruction>()
  -> variant-specific processor
```

`programs/magicblock/src/magicblock_processor.rs` is the dispatch source of truth for current variant behavior. Adding a variant or changing variant fields requires updating dispatch, helper builders, tests, and this guide. Reordering variants changes bincode discriminants and must be treated as a wire compatibility break.

### Intent bundle scheduling flow

1. Application/test program builds `MagicIntentBundleArgs` or legacy `MagicBaseIntentArgs` with compact account indices.
2. The caller invokes `MagicBlockInstruction::ScheduleIntentBundle(args)` or `ScheduleBaseIntent(args)` against the Magic Program and passes payer, `MAGIC_CONTEXT_PUBKEY`, optional fee vault, and referenced accounts.
3. `process_schedule_intent_bundle` verifies the MagicContext account, payer signer, parent program, slot/blockhash, and fee-vault/commit-limit path.
4. `MagicIntentBundle::try_from_args` resolves indices to accounts, builds committed-account/action payloads, rejects empty bundles and duplicate committed pubkeys, and distinguishes secure (`ScheduleIntentBundle`) from legacy (`ScheduleBaseIntent`) action source handling.
5. Commit-and-undelegate variants mark affected local accounts as undelegating/immutable before the intent is written.
6. The scheduled intent is written into MagicContext and the precomputed `ScheduledCommitSent` signature is logged.

Keep the argument structs compact and index-based. Changing them affects application CPI code, Magic Program validation, and committor/account recovery flows.

### Clone and program-materialization flow

```text
magicblock-account-cloner
  -> MagicBlockInstruction::{CloneAccount, CloneAccountInit, CloneAccountContinue, CleanupPartialClone, Finalize*ProgramFromBuffer, SetProgramAuthority}
  -> programs/magicblock/src/clone_account/* processors
  -> local AccountsDb/program state
```

`AccountCloneFields` carries the non-data account properties for clone installation. It must stay aligned with cloner request construction and Magic Program clone processors. Post-delegation actions use both clone instruction fields and `PostDelegationActionExecutorInstruction::Execute` in the same transaction; the post-action executor checks the immediately previous Magic Program clone instruction.

### Ephemeral account flow

1. Caller invokes `CreateEphemeralAccount { data_len }`, `ResizeEphemeralAccount { new_data_len }`, or `CloseEphemeralAccount`.
2. The instruction account list includes sponsor, ephemeral account, and `EPHEMERAL_VAULT_PUBKEY`.
3. `programs/magicblock/src/ephemeral_accounts` applies rent math using `EPHEMERAL_RENT_PER_BYTE` and account static-size overhead.
4. Processor tests assert sponsor/vault lamport movement, signer requirements, PDA sponsor rules, and close/resize behavior.

Changing the rent constant, vault pubkey, or account-meta shape affects user-visible balance semantics and tests.

### Task scheduling flow

```text
program CPI
  -> MagicBlockInstruction::{ScheduleTask, CancelTask}
  -> programs/magicblock/src/schedule_task/*
  -> ExecutionTlsStash registers TaskRequest
  -> magicblock-task-scheduler receives schedule/cancel request
  -> MagicBlockInstruction::ExecuteCrank under CRANK_PROGRAM_ID executes due instructions
```

`ScheduleTaskArgs` is the user-facing instruction payload; `TaskRequest` is the internal side-effect payload. Preserve both when changing scheduled task behavior.

### Callback result flow

1. A secure intent bundle can attach an action callback with `AddActionCallbackArgs`.
2. `process_add_action_callback` validates the latest intent, same payer, same slot/blockhash, source program, fee vault, and callback account metas.
3. Committor/service code reports action results through `ActionsCallbackService`.
4. `magicblock-services` builds a callback executor instruction under `CALLBACK_PROGRAM_ID`, wraps the destination instruction in `CallbackInstruction::ExecuteCallback`, and bincode-encodes `MagicResponse::V1` after the destination discriminator.
5. Callback programs deserialize `MagicResponse` and can check `CALLBACK_SIGNER` as the authorized PDA signer.

Do not add user-controlled signer bits to `ShortAccountMeta`; callback signer handling is intentionally derived from `CALLBACK_SIGNER`.

## Important internals and caveats

### Bincode compatibility

All major public structs/enums derive `Serialize` and `Deserialize` and are serialized with `bincode`. Enum variant order and struct field order matter. Adding fields without compatibility handling, removing variants, or reordering variants can break old transactions, persisted contexts, integration programs, or callback decoders.

The `Unused` instruction variant is intentionally retained as an unused slot after a removed `ScheduleCommitFinalize` path. Do not delete or repurpose it casually; doing so changes discriminants or semantics.

### Secure vs legacy action scheduling

`ScheduleBaseIntent(MagicBaseIntentArgs)` is the legacy single-intent path and is processed with `secure = false`. `ScheduleIntentBundle(MagicIntentBundleArgs)` is the recommended path and is processed with `secure = true`, allowing action source-program validation and callback attachment. Keep these distinctions aligned with `programs/magicblock/src/magic_scheduled_base_intent.rs` and `process_add_action_callback.rs`.

### `ShortAccountMeta` intentionally omits signer state

Base actions and callbacks carry compact account metas without `is_signer`. Users cannot request arbitrary signer privileges. The only callback signer is derived internally when the account pubkey equals `CALLBACK_SIGNER`; post-delegation and callback executor wrappers also clear signer bits on outer metas where needed.

### Fixed accounts are also reset/blacklist inputs

`MAGIC_CONTEXT_PUBKEY`, `EPHEMERAL_VAULT_PUBKEY`, Magic Program IDs, callback/crank/post-action program IDs, and validator IDs are protected from ordinary clone/reset paths in `magicblock-chainlink` and `magicblock-accounts-db`. Update those consumers when adding or changing fixed Magic Program accounts.

### No crate-local tests

This crate currently has no local unit tests. Behavior is validated through consumers (`programs/magicblock`, `magicblock-processor`, services, and integration tests). API changes should therefore include targeted consumer tests, not only `cargo nextest run -p magicblock-magic-program-api`.

## Important invariants

1. `MagicBlockInstruction`, `CallbackInstruction`, `PostDelegationActionExecutorInstruction`, and public argument/response structs must remain bincode-compatible unless the change is an intentional protocol migration with all consumers updated.
2. Program IDs, fixed account pubkeys, PDA seeds, and PDA derivations must remain stable across validator, Magic Program, services, tests, reset/blacklist logic, and application CPI callers.
3. `ShortAccountMeta` must not grow a user-controlled signer flag for base actions/callbacks without a security review and corresponding Magic Program validation changes.
4. Clone metadata must preserve `lamports`, `owner`, `executable`, `delegated`, `confined`, and `remote_slot`; missing or reordered fields can corrupt local clone semantics.
5. Commit and undelegation argument indices must refer to instruction accounts and must remain compact `u8` indices unless all builders and validators are updated.
6. Intent bundles must continue to reject empty bundles and duplicate committed-account pubkeys in the Magic Program implementation; API changes must not bypass those checks.
7. `ScheduleBaseIntent` and `ScheduleIntentBundle` must preserve their legacy/secure behavior distinction.
8. `EPHEMERAL_RENT_PER_BYTE` and `EPHEMERAL_VAULT_PUBKEY` changes are user-visible balance/lifecycle changes and require processor/integration validation.
9. Callback response payloads must remain decodable by callback programs that expect discriminator-prefixed bincode `MagicResponse` data.
10. The `backward-compat` feature must keep public type aliases coherent; do not expose mixed Solana major-version types in one public payload.

## Common change areas and what to inspect

### Adding or changing a Magic Program instruction

Start with:

- `magicblock-magic-program-api/src/instruction.rs`;
- `programs/magicblock/src/magicblock_processor.rs`;
- `programs/magicblock/src/utils/instruction_utils.rs`;
- relevant processor module under `programs/magicblock/src/`;
- call sites in `magicblock-account-cloner`, `magicblock-services`, `programs/guinea`, and integration programs.

Check account metas, signer/writable bits, bincode compatibility, discriminant impact, and whether `Unused` must remain in place.

### Changing commit/action/undelegation args

Inspect:

- `magicblock-magic-program-api/src/args.rs`;
- `programs/magicblock/src/magic_scheduled_base_intent.rs`;
- `programs/magicblock/src/schedule_transactions/process_schedule_intent_bundle.rs`;
- `programs/magicblock/src/schedule_transactions/process_add_action_callback.rs`;
- `magicblock-core/src/intent.rs`;
- `test-integration/programs/flexi-counter/src/processor/schedule_intent.rs`;
- `test-integration/schedulecommit/` suites.

Preserve compact index resolution, duplicate-account checks, fee behavior, callback source authorization, and undelegation immutability.

### Changing clone/program clone payloads

Inspect:

- `magicblock-magic-program-api/src/instruction.rs` (`AccountCloneFields` and clone variants);
- `magicblock-account-cloner/src/lib.rs`;
- `programs/magicblock/src/clone_account/`;
- `programs/magicblock/src/utils/instruction_utils.rs`;
- `magicblock-processor/tests/post_delegation_actions.rs`;
- `test-integration/test-cloning/`.

Do not drop `remote_slot`, delegation/confined flags, executable state, or post-delegation action handling.

### Changing ephemeral account API/constants

Inspect:

- `magicblock-magic-program-api/src/lib.rs`;
- `programs/magicblock/src/ephemeral_accounts/`;
- `programs/guinea/src/lib.rs` helper CPIs;
- `magicblock-api/src/fund_account.rs`;
- `magicblock-processor/tests/ephemeral_accounts.rs`.

Validate sponsor/vault lamports, account ownership, signer requirements, PDA sponsor handling, and close/resize refunds.

### Changing task payloads or crank PDAs

Inspect:

- `magicblock-magic-program-api/src/args.rs` and `src/pda.rs`;
- `programs/magicblock/src/schedule_task/`;
- `magicblock-core/src/tls.rs`;
- `magicblock-task-scheduler`;
- `programs/magicblock/src/utils/instruction_utils.rs`.

Preserve task IDs, authority semantics, TLS drainage expectations, and `CRANK_PROGRAM_ID` account metas.

### Changing callback responses or callback PDA behavior

Inspect:

- `magicblock-magic-program-api/src/response.rs` and `src/pda.rs`;
- `magicblock-services/src/actions_callback_service.rs`;
- `programs/magicblock/src/schedule_transactions/process_add_action_callback.rs`;
- callback consumers such as `test-integration/programs/flexi-counter/src/processor/callback.rs`.

Preserve discriminator-prefixing, `MagicResponse::V1` decoding, `CALLBACK_SIGNER` semantics, and callback fee/source validation.

### Changing Solana compatibility support

Inspect:

- `magicblock-magic-program-api/Cargo.toml`;
- `magicblock-magic-program-api/src/compat.rs`;
- any SDK/test-program builds that enable `backward-compat`.

Run builds/tests with and without `--features backward-compat` when public type aliases or dependencies change.

## Tests and validation

For documentation-only changes to this guide:

```bash
test -f agents/crates/magicblock-magic-program-api.md
rg "magicblock-magic-program-api.md" agents/04_crate-map.md
```

For crate changes, minimum targeted checks:

```bash
cargo fmt
cargo nextest run -p magicblock-magic-program-api
cargo nextest run -p magicblock-magic-program-api --features backward-compat
```

Because this crate has no local tests, also run targeted consumer tests for the changed API area:

```bash
cargo nextest run -p magicblock-program
cargo nextest run -p magicblock-processor ephemeral_accounts
cargo nextest run -p magicblock-processor post_delegation_actions
```

For commit/action/intent changes, prefer integration coverage:

```bash
cd test-integration
make test-schedule-intents
make test-committor-intent-bundles
```

For clone/program-clone API changes:

```bash
cd test-integration
make test-cloning
```

For broad validation, follow `agents/05_testing-and-validation.md`:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance expectations: this crate has no runtime loops, but its payload shape affects transaction size, serialization cost, account-meta count, scheduler pressure, clone chunking, and callback/base-action delivery. Report any API change that increases serialized instruction size or account requirements, and run the smallest relevant integration test that exercises the affected hot path.

## Related docs

- `agents/02_specification.md` for Magic Program, commit, undelegation, Magic Actions, cloning, task, and ephemeral account behavior.
- `agents/03_architecture.md` for the Magic Program scheduling versus validator-side settlement boundary.
- `agents/04_crate-map.md` for crate ownership and consumers.
- `agents/05_testing-and-validation.md` for validation workflow and integration-test commands.
- `agents/crates/magicblock-core.md` for `TaskRequest`, `BaseActionCallback`, and TLS/action callback contracts.
- `agents/crates/magicblock-account-cloner.md` for clone/program-clone flows that emit this crate's instruction payloads.
- `programs/magicblock/src/magicblock_processor.rs` and `programs/magicblock/src/utils/instruction_utils.rs` for current instruction interpretation and helper builders.
