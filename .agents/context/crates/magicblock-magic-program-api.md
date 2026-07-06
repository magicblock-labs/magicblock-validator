# `magicblock-magic-program-api`

## Purpose

`magicblock-magic-program-api` is the shared wire-contract crate for the Magic Program and its built-in companion programs. It defines the program IDs, fixed accounts, PDA helpers, instruction enums, scheduling argument structs, clone metadata, task request payloads, callback response payloads, and Solana-type compatibility exports used by the validator and test programs.

This crate is small, but it is protocol- and compatibility-sensitive. Its current types derive wincode schemas and use bincode-compatible wincode encoding in transactions, persisted scheduled intents, executor TLS payloads, and callback instructions. The opt-in `backward-compat` feature retains serde/bincode for Solana 2.x types that do not expose wincode schemas. Changes can affect Magic Program CPI callers, account cloning, task scheduling, callback delivery, committor intent construction, account reset/blacklist behavior, and integration tests. Treat enum variant order, field order, constants, and feature-gated public type aliases as wire/API contracts.

End-to-end commit/undelegation semantics live in .agents/specs/validator-specification.md; this crate owns shared Magic Program wire types, intent/callback payloads, program IDs, and PDA helpers consumed by settlement and callback code.

High-level responsibilities:

- expose the Magic Program ID (`Magic111...`) plus fixed companion IDs/accounts such as `MAGIC_CONTEXT_PUBKEY`, `EPHEMERAL_VAULT_PUBKEY`, `CRANK_PROGRAM_ID`, and `CALLBACK_PROGRAM_ID`;
- define `MagicBlockInstruction` and `CallbackInstruction` payloads consumed by `programs/magicblock` and validator-built internal transactions;
- define commit/action/undelegation bundle argument types used by application programs and Magic Program scheduling processors;
- define task scheduling/cancel request payloads sent from Magic Program execution into `magicblock-core` TLS and the task scheduler;
- define callback response types (`MagicResponse`, `MagicResponseV1`, `ActionReceipt`) delivered to base-layer callback programs;
- provide a `compat` boundary so public API users can opt into Solana 2.x-compatible types through the `backward-compat` feature while the default uses workspace Solana 3.x types.

Do not put Magic Program execution logic, validation policy, persistence, RPC behavior, or committor delivery logic in this crate. Those belong in `programs/magicblock`, `magicblock-core`, `magicblock-accounts`, and the committor/service crates.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-magic-program-api` change. In particular, update it for changes to:

- any public constant, program ID, fixed account, PDA seed, PDA derivation, or rent/context-size constant;
- `MagicBlockInstruction`, `CallbackInstruction`, `AccountCloneFields`, account-modification types, or serialization behavior;
- commit, action, undelegation, intent-bundle, callback, task, or `ShortAccountMeta` argument structs/enums;
- the `backward-compat` feature or `compat` module public type aliases;
- callback response payload shape or receipt semantics;
- account metas expected by Magic Program processors or helper builders in `programs/magicblock/src/utils/instruction_utils.rs`;
- validation commands that should be run after API changes.


For the general documentation-update rule, see .agents/memory/agent-memory-and-docs.md.

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
| `magicblock-services/src/actions_callback_service.rs` | Builds `CallbackInstruction::ExecuteCallback` and wincode-encoded `MagicResponse` values. |
| `magicblock-chainlink/src/cloner/mod.rs` | Materializes fetched accounts through the engine accessor and forwards post-delegation actions to MagicRoot. |
| `../engine/engine/src/accessor.rs` | Composes account creation and optional post-finalize actions through MagicRoot. |

Main consumers include:

- `programs/magicblock`, the runtime implementation that deserializes and executes these instructions;
- `magicblock-core`, which stores task requests and callback/action payload dependencies;
- `magicblock-chainlink`, `magicblock-api`, and `magicblock-services`;
- consumer and service tests backed by the engine testkit.

## Public API shape / Main public types and APIs

### Root exports and constants

`src/lib.rs` publicly exports `args`, `compat`, `instruction`, `pda`, and `response`, then re-exports `compat::{declare_id, pubkey, Pubkey}`. The crate declares the Magic Program ID and fixed companion accounts/programs:

- `ID` / `id()` from `declare_id!("Magic11111111111111111111111111111111111111")`;
- `CRANK_PROGRAM_ID` for task/crank execution;
- `CALLBACK_PROGRAM_ID` for callback executor built-in instructions;
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

- `MagicBlockInstruction`: the primary wincode-serialized instruction enum for the Magic Program. Variants cover account modification, legacy and bundled commit scheduling, task scheduling/canceling, executable-check toggles, no-op uniqueness, ephemeral account create/resize/close, clone/chunk/cleanup, program finalization, callback attachment, account eviction, and crank execution.
- `CallbackInstruction`: instruction enum for the callback executor built-in. Current variant is `ExecuteCallback { instruction }`.
- `AccountCloneFields`: clone metadata (`lamports`, `owner`, `executable`, `delegated`, `confined`, `remote_slot`) that must preserve remote/local account semantics across clone instructions.
- `AccountModification` and `AccountModificationForInstruction`: validator-only account flag/owner modifications.

`MagicBlockInstruction::try_to_vec()` uses wincode by default. Under `backward-compat` it retains bincode serialization because the Solana 2.x compatibility types do not implement wincode schemas. Current call sites use `Instruction::new_with_wincode`.

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
- `MagicResponseV1 { ok, data, error, receipt }`, wincode-encoded after a callback-specific discriminator by `magicblock-services`;
- `ActionReceipt { signature }`, present when the base action transaction signature is available.

Callback programs deserialize this payload directly.

## Runtime flows

### Magic Program instruction dispatch

```text
caller / validator helper
  -> Instruction::new_with_wincode(program_id, &MagicBlockInstruction::..., metas)
  -> programs/magicblock/src/magicblock_processor.rs
  -> wincode::deserialize::<MagicBlockInstruction>()
  -> variant-specific processor
```

`programs/magicblock/src/magicblock_processor.rs` is the dispatch source of truth for current variant behavior. Adding a variant or changing variant fields requires updating dispatch, helper builders, tests, and this guide. Wincode preserves the existing bincode-compatible layout, so reordering variants changes wire discriminants and must be treated as a compatibility break.

### Intent bundle scheduling flow

1. Application/test program builds `MagicIntentBundleArgs` or legacy `MagicBaseIntentArgs` with compact account indices.
2. The caller invokes `MagicBlockInstruction::ScheduleIntentBundle(args)` or `ScheduleBaseIntent(args)` against the Magic Program and passes payer, `MAGIC_CONTEXT_PUBKEY`, optional fee vault, and referenced accounts.
3. `process_schedule_intent_bundle` verifies the MagicContext account, payer signer, parent program, slot/blockhash, and fee-vault/commit-limit path.
4. `MagicIntentBundle::try_from_args` resolves indices to accounts, builds committed-account/action payloads, rejects empty bundles and duplicate committed pubkeys, and distinguishes secure (`ScheduleIntentBundle`) from legacy (`ScheduleBaseIntent`) action source handling.
5. Commit-and-undelegate variants mark affected local accounts as undelegating/immutable before the intent is written.
6. The scheduled intent is written into MagicContext and the precomputed `ScheduledCommitSent` signature is logged.

Keep the argument structs compact and index-based. Changing them affects application CPI code, Magic Program validation, and committor/account recovery flows.

### Account and program materialization

```text
magicblock-chainlink
  -> Engine::account(pubkey).create(account, actions)
  -> MagicRootInstruction::{Patch, Finalize, PostFinalize}
  -> engine accountsdb/program state
```

MagicRoot composes account patches, finalization, and optional post-delegation
actions into one engine transaction. The legacy Magic Program clone variants and
their discriminants remain in `MagicBlockInstruction` for wire compatibility,
but the processor rejects them with `AccountCompositionRemoved`; do not reuse or
renumber those variants.

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
4. `magicblock-services` builds a callback executor instruction under `CALLBACK_PROGRAM_ID`, wraps the destination instruction in `CallbackInstruction::ExecuteCallback`, and wincode-encodes `MagicResponse::V1` after the destination discriminator.
5. Callback programs deserialize `MagicResponse` and can check `CALLBACK_SIGNER` as the authorized PDA signer.

Do not add user-controlled signer bits to `ShortAccountMeta`; callback signer handling is intentionally derived from `CALLBACK_SIGNER`.

## Important internals and caveats

### Wire compatibility

Current public structs/enums retain serde derives and also derive `SchemaRead`/`SchemaWrite`; wincode's default encoding remains byte-compatible with the prior bincode representation. Enum variant order and struct field order still matter. Under `backward-compat`, schema derives are disabled and `try_to_vec` uses bincode for Solana 2.x types.

The `Unused` instruction variant is intentionally retained as an unused slot after a removed `ScheduleCommitFinalize` path. Do not delete or repurpose it casually; doing so changes discriminants or semantics.

### Secure vs legacy action scheduling

`ScheduleBaseIntent(MagicBaseIntentArgs)` is the legacy single-intent path and is processed with `secure = false`. `ScheduleIntentBundle(MagicIntentBundleArgs)` is the recommended path and is processed with `secure = true`, allowing action source-program validation and callback attachment. Keep these distinctions aligned with `programs/magicblock/src/magic_scheduled_base_intent.rs` and `process_add_action_callback.rs`.

### `ShortAccountMeta` intentionally omits signer state

Base actions and callbacks carry compact account metas without `is_signer`. Users cannot request arbitrary signer privileges. The only callback signer is derived internally when the account pubkey equals `CALLBACK_SIGNER`. MagicRoot clears signer bits from outer post-finalize metas and vouches only for signers declared by each nested action.

### Fixed accounts are also reset/blacklist inputs

`MAGIC_CONTEXT_PUBKEY`, `EPHEMERAL_VAULT_PUBKEY`, Magic Program IDs, callback/crank program IDs, and validator IDs are protected from ordinary materialization paths in `magicblock-chainlink`. Update that consumer when adding or changing fixed Magic Program accounts.

### No crate-local tests

This crate currently has no local unit tests. Behavior is validated through consumers (`programs/magicblock`, Chainlink, services, and integration tests). API changes should therefore include targeted consumer validation, not only this crate's package checks.

## Important invariants

1. `MagicBlockInstruction`, `CallbackInstruction`, and public argument/response structs must remain byte-compatible with their established encoding unless the change is an intentional protocol migration with all consumers updated.
2. Program IDs, fixed account pubkeys, PDA seeds, and PDA derivations must remain stable across validator, Magic Program, services, tests, reset/blacklist logic, and application CPI callers.
3. `ShortAccountMeta` must not grow a user-controlled signer flag for base actions/callbacks without a security review and corresponding Magic Program validation changes.
4. Clone metadata must preserve `lamports`, `owner`, `executable`, `delegated`, `confined`, and `remote_slot`; missing or reordered fields can corrupt local clone semantics.
5. Commit and undelegation argument indices must refer to instruction accounts and must remain compact `u8` indices unless all builders and validators are updated.
6. Intent bundles must continue to reject empty bundles and duplicate committed-account pubkeys in the Magic Program implementation; API changes must not bypass those checks.
7. `ScheduleBaseIntent` and `ScheduleIntentBundle` must preserve their legacy/secure behavior distinction.
8. `EPHEMERAL_RENT_PER_BYTE` and `EPHEMERAL_VAULT_PUBKEY` changes are user-visible balance/lifecycle changes and require processor/integration validation.
9. Callback response payloads must remain decodable by callback programs that expect the established discriminator-prefixed `MagicResponse` encoding.
10. The `backward-compat` feature must keep public type aliases coherent; do not expose mixed Solana major-version types in one public payload.

## Common change areas and what to inspect

### Adding or changing a Magic Program instruction

Start with:

- `magicblock-magic-program-api/src/instruction.rs`;
- `programs/magicblock/src/magicblock_processor.rs`;
- `programs/magicblock/src/utils/instruction_utils.rs`;
- relevant processor module under `programs/magicblock/src/`;
- call sites in `magicblock-chainlink`, `magicblock-services`, and integration programs.

Check account metas, signer/writable bits, wire compatibility, discriminant impact, and whether `Unused` must remain in place.

### Changing commit/action/undelegation args

Inspect:

- `magicblock-magic-program-api/src/args.rs`;
- `programs/magicblock/src/magic_scheduled_base_intent.rs`;
- `programs/magicblock/src/schedule_transactions/process_schedule_intent_bundle.rs`;
- `programs/magicblock/src/schedule_transactions/process_add_action_callback.rs`;
- `magicblock-core/src/intent.rs`;

Preserve compact index resolution, duplicate-account checks, fee behavior, callback source authorization, and undelegation immutability.

### Changing legacy clone/program clone payloads

Inspect:

- `magicblock-magic-program-api/src/instruction.rs` (`AccountCloneFields` and clone variants);
- `magicblock-chainlink/src/cloner/mod.rs`;
- `../engine/engine/src/accessor.rs` and MagicRoot;
- `programs/magicblock/src/utils/instruction_utils.rs`;

The legacy variants are unsupported but wire-stable. Do not renumber or repurpose
them. Live materialization must preserve slot, account mode, executable state,
and post-delegation action handling through the engine.

### Changing ephemeral account API/constants

Inspect:

- `magicblock-magic-program-api/src/lib.rs`;
- `programs/magicblock/src/ephemeral_accounts/`;
- engine-testkit-backed processor coverage;
- `magicblock-api/src/fund_account.rs`;
- focused Magic Program and engine-testkit consumer coverage.

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

Preserve discriminator-prefixing, `MagicResponse::V1` decoding, `CALLBACK_SIGNER` semantics, and callback fee/source validation.

### Changing Solana compatibility support

Inspect:

- `magicblock-magic-program-api/Cargo.toml`;
- `magicblock-magic-program-api/src/compat.rs`;
- any SDK/test-program builds that enable `backward-compat`.

Run builds/tests with and without `--features backward-compat` when public type aliases or dependencies change.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-magic-program-api`, including the `backward-compat` feature when public type aliases change.
- Consumer validation intent: because this crate has no local tests, include affected consumer packages such as `magicblock-program`, `magicblock-chainlink`, and `magicblock-api`.
- Performance/security validation intent: report API changes that increase serialized instruction size or account requirements, and validate the smallest relevant hot path.


## Adjacent implementation references

- `.agents/context/crates/magicblock-core.md` — `TaskRequest`, `BaseActionCallback`, and TLS/action callback contracts.
- `.agents/context/crates/magicblock-chainlink.md` — account synchronization and engine materialization flow.
- `.agents/context/crates/magicblock-services.md` — callback instruction and response delivery.
- `programs/magicblock/src/magicblock_processor.rs` and `programs/magicblock/src/utils/instruction_utils.rs` — current instruction interpretation and helper builders.
