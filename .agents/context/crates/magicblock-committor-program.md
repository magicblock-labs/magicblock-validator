# `magicblock-committor-program`

## Purpose

`magicblock-committor-program` is the base-layer helper program and shared Rust API used by the committor service to upload large commit payloads into temporary on-chain buffer accounts. The Delegation Program commit/finalize instructions can then read those buffers when account data or diffs are too large to fit directly in one transaction.

High-level responsibilities:

- define the committor program id `ComtrB2KEaWgXsW1dhr1xYL4Ht4Bjj3gXnnL6KMdABq` and Solana entrypoint;
- derive deterministic validator-authority-scoped `chunks` and `buffer` PDAs for `(authority, delegated account pubkey, commit_id)`;
- initialize temporary buffer/chunk-tracker accounts, grow buffers over multiple instructions, write account-data chunks, and close/refund temporary accounts;
- provide client-side instruction builders used by `magicblock-committor-service`;
- provide shared `Changeset`, `ChangedAccount`, `CommitableAccount`, `Chunks`, and `ChangesetChunks` types used to split account changes into retryable chunks.

This crate is on the base-layer settlement path. Its on-chain processor is compute- and transaction-size-sensitive, and its exported wire formats/PDA seeds are compatibility-sensitive. Changes can affect commit delivery, retry/recovery behavior, and whether large state commits fit in Solana transactions.

End-to-end commit/undelegation semantics live in .agents/specs/validator-specification.md; this crate owns the on-chain buffer/chunk PDA program and client instruction builders used by committor buffer delivery.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-committor-program` change. In particular, update it for changes to:

- `CommittorInstruction` variants, account order, Borsh layout, instruction-size constants, or program id;
- PDA seed strings, seed order, bump handling, authority scoping, or `commit_id` encoding;
- signer requirements, account-allocation checks, close/refund behavior, or buffer/chunks ownership assumptions;
- max allocation, max instruction count, max instruction data size, write chunk sizing, or realloc chunking behavior;
- `Changeset`, `ChangedAccount`, `CommitableAccount`, `Chunks`, `ChangesetChunks`, or bundle/undelegation metadata semantics;
- instruction-builder APIs consumed by `magicblock-committor-service`;
- committor-service buffer preparation, retry, cleanup, or recovery flows that rely on this crate.


For the general documentation-update rule, see .agents/memory/agent-memory-and-docs.md.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-committor-program/Cargo.toml` | Package metadata, Solana/Borsh dependencies, `cdylib`/`lib` crate types, and `no-entrypoint`, `custom-heap`, `custom-panic` features. |
| `magicblock-committor-program/src/lib.rs` | Public crate surface, program id declaration, entrypoint registration, processor export, and shared state/type re-exports. |
| `magicblock-committor-program/src/instruction.rs` | `CommittorInstruction` wire enum plus instruction-size constants used for transaction packing. |
| `magicblock-committor-program/src/processor.rs` | On-chain instruction processor for `Init`, `ReallocBuffer`, `Write`, and `Close`. Enforces signer and PDA checks. |
| `magicblock-committor-program/src/pdas.rs` | PDA seed helpers and `verified_seeds_and_pda!` macro for chunks/buffer accounts. |
| `magicblock-committor-program/src/instruction_builder/` | Client-side builders for init, realloc, write, and close instructions. Main consumer is `magicblock-committor-service`. |
| `magicblock-committor-program/src/instruction_chunks.rs` | Splits init/realloc instructions into transaction-sized groups. |
| `magicblock-committor-program/src/state/changeset.rs` | Shared account-change model, metadata extraction, bundling, undelegation markers, and `CommitableAccount` chunk iteration. |
| `magicblock-committor-program/src/state/chunks.rs` | Borsh-serialized bitfield that tracks which buffer chunks have landed. |
| `magicblock-committor-program/src/state/changeset_chunks.rs` | Iterators that turn account data/diffs into `ChangesetChunk { offset, data_chunk }` values and retry only missing chunks. |
| `magicblock-committor-program/src/utils/` | Low-level program assertions and close/refund helper. |
| `magicblock-committor-program/bin/magicblock_committor_program.so` | Checked-in program binary artifact used by validator/test setup. Treat as a deployment artifact, not source documentation. |
| `magicblock-committor-service/src/tasks/commit_task.rs` and `commit_finalize_task.rs` | Build buffer preparation stages and derive buffer PDAs consumed by Delegation Program commit/finalize-from-buffer instructions. |
| `magicblock-committor-service/src/transaction_preparator/delivery_preparator.rs` | Sends init/realloc/write instructions, retries missing chunks, handles cleanup, and persists buffer-preparation status. |
| `test-integration/test-committor-service/` | Integration coverage for committor delivery preparation, transactions, intent execution, and PDA/buffer behavior. |

Main consumers:

- `magicblock-committor-service`, which re-exports `ChangedAccount`, `Changeset`, and `ChangesetMeta`, builds buffer tasks, and sends committor-program instructions;
- `magicblock-accounts`, which uses `ChangesetMeta` in scheduled-commit error paths;
- integration tests under `test-integration/test-committor-service` and config tests that allow/deny the committor program id;
- the workspace root, which depends on this crate with `features = ["no-entrypoint"]` for client-side/library use.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exports:

- modules: `consts`, `error`, `instruction`, `instruction_chunks`, `pdas`, and `instruction_builder`;
- `process` for the Solana entrypoint/processor;
- shared state types: `ChangedAccount`, `ChangedAccountMeta`, `ChangedBundle`, `Changeset`, `ChangesetBundles`, `ChangesetMeta`, `CommitableAccount`, `ChangesetChunk`, `ChangesetChunks`, and `Chunks`.

### Instructions and builders

`CommittorInstruction` currently has four Borsh-serialized variants:

1. `Init` creates the chunks PDA and buffer PDA. The buffer is rent-funded for the final desired size but initially allocated only up to `MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE` bytes.
2. `ReallocBuffer` grows the buffer account by at most `MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE` bytes per invocation until it reaches the requested size.
3. `Write` copies one `data_chunk` into the buffer at `offset` and marks the corresponding offset as delivered in `Chunks`.
4. `Close` zero-resizes and refunds both temporary accounts to the validator authority.

The instruction builders in `src/instruction_builder/` derive PDAs and return ordinary `Instruction` values:

- `create_init_ix(CreateInitIxArgs) -> (Instruction, chunks_pda, buffer_pda)`;
- `create_realloc_buffer_ixs(CreateReallocBufferIxArgs) -> Vec<Instruction>`;
- `create_write_ix(CreateWriteIxArgs) -> Instruction`;
- `create_close_ix(CreateCloseIxArgs) -> Instruction`.

The account order documented in `CommittorInstruction` and emitted by these builders is part of the contract with `processor.rs`; keep them synchronized.

### PDAs

PDA derivation is deterministic and scoped by validator authority:

```text
chunks seeds = [committor_program_id, b"comittor_chunks", authority, account_pubkey, commit_id_le_bytes]
buffer seeds = [committor_program_id, b"comittor_buffer", authority, account_pubkey, commit_id_le_bytes]
```

The seed strings intentionally use `comittor_*` spelling as implemented. Do not rename or correct them without a migration plan for all buffer producers/consumers.

### Changeset and chunk types

- `Changeset` stores committed accounts, the ER slot that requested the commit, and accounts to undelegate after commit.
- `ChangedAccount::Full` stores lamports, full data, original delegated-account owner, and `bundle_id`. `ChangedAccount::Diff` is only a placeholder; current methods `unreachable!` on it.
- `ChangesetMeta` extracts cheap per-account metadata for diagnostics without cloning full account data.
- `Changeset::into_committables(chunk_size)` creates one `CommitableAccount` per account and preserves undelegation and bundle metadata.
- `Chunks` is a compact Borsh bitfield of delivered chunks. `Chunks::new` asserts the serialized tracker fits within one allocation instruction.
- `ChangesetChunks` iterates all chunks or only missing chunks for retry.

## Runtime flows

### Buffer delivery flow

```text
CommitTask/CommitFinalizeTask chooses *InBuffer delivery
  -> PreparationTask creates Chunks from buffer_data and MAX_WRITE_CHUNK_SIZE
  -> DeliveryPreparator::initialize_buffer_account
     -> create_init_ix
     -> create_realloc_buffer_ixs
     -> chunk_realloc_ixs groups init/reallocs into transaction-sized batches
     -> send init, then send realloc batches in parallel
  -> DeliveryPreparator::write_buffer_with_retries
     -> PreparationTask::write_instructions
     -> create_write_ix for every ChangesetChunk
     -> retry missing chunks using on-chain Chunks state
  -> Delegation Program commit/finalize-from-buffer reads buffer PDA
  -> create_close_ix cleanup closes chunks/buffer and refunds authority
```

The committor service persists intermediate buffer statuses around this flow. If initialization finds already-initialized accounts, it attempts cleanup, invalidates the cached blockhash, restores preparation stage, and retries once.

### On-chain `Init`

1. Requires exactly `[authority signer, chunks writable, buffer writable, system_program]`.
2. Verifies program id and derives both PDAs from the supplied authority, target account pubkey, commit id, and bumps.
3. Requires both PDAs to be unallocated.
4. Creates the chunks account at the requested chunks-account size.
5. Creates the buffer account with rent for the full requested size but data length capped at `MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE`.
6. Serializes an empty `Chunks::new(chunk_count, chunk_size)` tracker into the chunks account.

### On-chain `ReallocBuffer`

1. Requires `[authority signer, buffer writable]` unless the buffer is already at least the requested size, in which case it returns `Ok(())` before signer/PDA checks.
2. Verifies the buffer PDA for the supplied authority/account/commit id/bump.
3. Grows data length by at most `MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE`, capped at the final buffer size.
4. Does not require extra rent or system program because `Init` pre-funded rent for the full desired buffer size.

### On-chain `Write`

1. Requires `[authority signer, chunks writable, buffer writable]`.
2. Verifies both PDAs.
3. Checks `offset + data_chunk.len()` for overflow and bounds against current buffer length.
4. Copies bytes into the buffer.
5. Deserializes `Chunks`, marks `offset / chunk_size` delivered, and writes the tracker back.

Offsets must be multiples of `chunk_size`; this is enforced through `Chunks::set_offset_delivered`.

### On-chain `Close`

1. Requires `[authority signer, chunks writable, buffer writable]`.
2. Verifies both PDAs.
3. Calls `close_and_refund_authority` on chunks and buffer accounts.
4. `close_and_refund_authority` resizes account data to zero before transferring lamports to mitigate refund/remaining-instruction account reuse attacks.

## Important internals and caveats

### Transaction-size constants

`consts.rs` and `instruction.rs` carry hand-tuned constants used by `magicblock-committor-service/src/consts.rs` to calculate `MAX_WRITE_CHUNK_SIZE`. `MAX_INSTRUCTION_DATA_SIZE` is based on empirical Solana transaction-size limits, while `IX_*_SIZE` constants approximate serialized instruction overhead. Changing these values can make buffer writes exceed transaction size or underutilize transaction capacity; validate with committor transaction-preparator tests and integration tests.

### Allocation and chunk tracker sizing

`MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE` is `10_240`. `Init` creates the initial buffer at this size or less; larger buffers require realloc instructions. `Chunks::new` panics if the chunk tracker itself would exceed one allocation instruction. This is treated as a programming/configuration bug, not a recoverable on-chain validation error.

### Retry model

The retry model assumes each successful `Write` updates the `Chunks` account atomically with the buffer write. The service can fetch chunks state, call `CommitableAccount::set_chunks`, and retry only `iter_missing()`. Do not decouple buffer writes from chunk tracking unless recovery and retry semantics are redesigned.

### Placeholder diffs in `ChangedAccount`

`ChangedAccount::Diff` exists as a placeholder but is not supported. Several accessors and conversions call `unreachable!` for diffs. Diff delivery currently happens in `magicblock-committor-service` by computing byte diffs for Delegation Program instructions, not by storing `ChangedAccount::Diff` in this crate.

### Authority and PDA scope

All temporary accounts are validator-authority scoped. The authority must sign all mutating instructions. This prevents one authority from modifying or closing another authority's buffers for the same delegated account and commit id. Do not relax signer checks or remove authority from seeds.

## Important invariants

1. The validator authority signer requirement for `Init`, `ReallocBuffer`, `Write`, and `Close` must be preserved.
2. Chunks and buffer PDAs must remain derived from program id, fixed seed string, authority pubkey, target account pubkey, and little-endian `commit_id`.
3. Instruction-builder account metas must match the account order expected by `processor.rs`.
4. `Init` must create only unallocated temporary accounts and must own them with the committor program id.
5. Buffer accounts must be rent-funded for the full requested size before no-rent reallocs are attempted.
6. Each `Write` must bounds-check offset and length before mutating buffer data.
7. Chunk delivery state must be updated only for offsets aligned to `chunk_size`.
8. Cleanup must verify PDAs before refunding lamports and must zero-resize accounts before lamport transfer.
9. Transaction-size and allocation constants must remain aligned with committor-service packing and compute-budget assumptions.
10. `ChangedAccount::Full.owner` must continue to mean the original owner of the delegated account on the base layer.
11. `bundle_id` and `accounts_to_undelegate` metadata must survive conversion into committable/bundled forms.
12. Do not introduce unbounded per-instruction work or excessive logging in the on-chain processor; every instruction is paid compute on the base layer.

## Common change areas and what to inspect

### Changing instruction layout or accounts

Start with `src/instruction.rs`, `src/processor.rs`, and `src/instruction_builder/`. Then inspect `magicblock-committor-service/src/tasks/mod.rs`, `transaction_preparator/delivery_preparator.rs`, and integration tests under `test-integration/test-committor-service`. Verify Borsh layouts, account order, signer flags, PDA derivation, and transaction-size constants.

### Changing buffer size, chunk size, or packing limits

Inspect `src/consts.rs`, `src/instruction.rs`, `src/instruction_chunks.rs`, `src/state/chunks.rs`, `src/state/changeset_chunks.rs`, and `magicblock-committor-service/src/consts.rs`. Validate that init/realloc batches still fit under transaction-size and instruction-trace limits, and that write instructions leave room for compute-budget instructions.

### Changing PDA derivation or authority behavior

Inspect `src/pdas.rs`, the `verified_seeds_and_pda!` macro, all instruction builders, `CommitTask::commit_state_from_buffer_ix`, and `CommitFinalizeTask::commit_finalize_from_buffer_ix`. PDA changes are wire-contract changes and require a migration/compatibility plan.

### Changing commit/change-set metadata

Inspect `src/state/changeset.rs`, `magicblock-committor-service/src/tasks/task_builder.rs`, `magicblock-accounts/src/errors.rs`, and any recovery/persistence code that stores commit metadata. Preserve owner, slot, undelegation, and bundle semantics.

### Changing cleanup behavior

Inspect `src/utils/account.rs`, `processor::process_close`, and `DeliveryPreparator::cleanup`. Preserve PDA verification, signer requirement, and zero-resize-before-refund behavior.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-committor-program` and, when public APIs or buffer preparation change, `magicblock-committor-service`.
- Relevant integration suites: `test-committor`, especially preparator, commit-finalize, and intent-executor targets; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Performance/security validation intent: report changes to buffer transaction count, chunk count, compute units, RPC sends, or retry rounds; confirm signer/PDA checks, transaction-size fit, retry, and cleanup behavior remain intact.


## Adjacent implementation references

- `magicblock-committor-service/README.md` — high-level intent-execution architecture notes for the main runtime consumer.
- `.agents/context/crates/magicblock-committor-service.md` — service-side buffer preparation, retry, cleanup, and recovery integration.
- `magicblock-committor-service/src/tasks/commit_task.rs` and `commit_finalize_task.rs` — buffer-task construction and Delegation Program commit/finalize-from-buffer call sites.
- `magicblock-committor-service/src/transaction_preparator/delivery_preparator.rs` — runtime buffer initialization, write retry, and cleanup call site.
- `test-integration/test-committor-service/` — integration coverage for committor delivery and intent execution.
