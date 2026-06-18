# `magicblock-account-cloner`

## Purpose

`magicblock-account-cloner` is the production implementation of the `magicblock-chainlink::cloner::Cloner` boundary. Chainlink decides which base-layer accounts or programs need to exist locally; this crate turns those clone requests into validator-signed local transactions that invoke the Magic Program and materialize the state inside the ER validator.

At a high level it:

- builds and submits Magic Program clone/evict instructions through the internal `TransactionSchedulerHandle`,
- encodes small accounts inline with `CloneAccount`,
- chunks large accounts with `CloneAccountInit` and `CloneAccountContinue`,
- executes post-delegation actions after delegated-account cloning,
- clones program ELF data through a temporary validator-owned buffer PDA and finalizes loader-specific program accounts,
- converts old BPF loader v1 programs into upgradeable-loader v3 local representation,
- deploys loader-v2/v3/v4 programs through the local loader-v4 path,
- provides small helpers for committor result diagnostics and deterministic program-clone buffer PDAs.

This crate is on the account-availability path for transaction submission, RPC read misses that need cloning, and program loading. Changes can affect account-sync latency, transaction-scheduler pressure, Magic Program instruction compatibility, program execution readiness, and post-delegation action safety. Avoid adding blocking work, duplicate clone transactions, unnecessary serialization, unbounded logging, or extra scheduler round trips in these flows without an explicit performance tradeoff.

## Update requirement

Update this document in the same change whenever behavior in `magicblock-account-cloner` changes, or whenever another crate changes the clone requests or Magic Program instructions this crate consumes. Update it for changes to:

- the `ChainlinkCloner` public constructor or its `Cloner` trait implementation,
- account clone sizing, chunking, cleanup, post-delegation action handling, or transaction-size limits,
- program clone flow, loader support, buffer PDA derivation, executable-check toggling, deploy/finalize authority handling, or one-slot wait behavior,
- Magic Program clone/evict/finalize instruction fields or account metas,
- `AccountCloneRequest`, `DelegationActions`, `LoadedProgram`, or `RemoteProgramLoader` semantics in `magicblock-chainlink`,
- error mapping or transaction diagnostics in `account_cloner.rs` / `util.rs`,
- tests or integration validation for account/program cloning,
- performance characteristics of clone transaction construction or scheduler submission.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

Primary files:

| Path | Role |
|---|---|
| `magicblock-account-cloner/Cargo.toml` | Crate dependencies. Depends on Chainlink for the `Cloner` trait/request types, Magic Program/API for clone instructions, ledger for latest blockhash/slot, core for scheduler handles, and committor/rpc-client for diagnostics helpers. |
| `magicblock-account-cloner/README.md` | High-level cloning notes. Some component names are historical; treat source and this agent guide as canonical for current implementation. |
| `magicblock-account-cloner/src/lib.rs` | Main `ChainlinkCloner` implementation, transaction builders, account clone flow, program clone flow, and unit tests. |
| `magicblock-account-cloner/src/account_cloner.rs` | `AccountClonerError`, result alias, and `map_committor_request_result` helper for turning committor oneshot results into diagnostic-rich errors. |
| `magicblock-account-cloner/src/util.rs` | Buffer PDA derivation and transaction diagnostic lookup helpers. |
| `magicblock-chainlink/src/cloner/mod.rs` | Trait boundary implemented here: `Cloner`, `AccountCloneRequest`, and `DelegationActions`. |
| `magicblock-chainlink/src/fetch_cloner/` | Builds clone requests, resolves delegation records/actions/program dependencies, and calls this crate through the `Cloner` trait. |
| `programs/magicblock/src/clone_account/` | Magic Program processors for clone, chunk, cleanup, post-delegation actions, and program finalization instructions emitted by this crate. |
| `magicblock-magic-program-api/src/instruction.rs` | Wire enum and `AccountCloneFields` used by clone instructions. |
| `magicblock-api/src/magic_validator.rs` | Production startup wiring: constructs `ChainlinkCloner` and passes it into `ProdInnerChainlink`. |
| `test-integration/test-cloning/` | Integration coverage for account/program cloning behavior, including multi-program and post-delegation action scenarios. |

Main consumers:

- `magicblock-api` creates `ChainlinkCloner::new(transaction_scheduler, latest_block)` during validator startup unless Chainlink is disabled for replica mode.
- `magicblock-chainlink` owns clone decisions and invokes this crate through `Arc<dyn Cloner>`-style generic wiring.
- `magicblock-accounts` aliases `ProdChainlink<ChainlinkCloner>` for scheduled commit / undelegation integration.
- `magicblock-aperture` stores the same production Chainlink alias in shared RPC state; it reaches the cloner indirectly through Chainlink.
- `magicblock-api` and `magicblock-accounts` convert `AccountClonerError` where committor diagnostic helpers are used.

Important upstream/downstream relationships:

- Upstream account classification, delegation resolution, post-delegation action validation, and program resolution happen in `magicblock-chainlink`; do not duplicate those policies here.
- Downstream state mutation happens by submitting local transactions to the Magic Program through `magicblock-core`'s transaction scheduler; this crate does not write `AccountsDb` directly.
- Program clone instructions must remain compatible with `programs/magicblock` processors and Solana loader interfaces.

## Public API shape / Main public types and APIs

The crate exports:

```rust
mod account_cloner;
mod util;

pub use account_cloner::*;
pub use util::derive_buffer_pubkey;

pub struct ChainlinkCloner { ... }
```

### `ChainlinkCloner`

`ChainlinkCloner` stores:

- `tx_scheduler: TransactionSchedulerHandle` — internal scheduler used to execute validator-signed local clone transactions.
- `block: LatestBlock` — latest local blockhash/slot source used for transaction signing and post-program-clone readiness waits.

Public constructor:

```rust
impl ChainlinkCloner {
    pub fn new(
        tx_scheduler: TransactionSchedulerHandle,
        block: LatestBlock,
    ) -> Self
}
```

Production wiring in `magicblock-api/src/magic_validator.rs` wraps it in `Arc` and passes it into `InnerChainlinkImpl::try_new_from_endpoints`.

### `Cloner` trait implementation

`ChainlinkCloner` implements `magicblock_chainlink::cloner::Cloner`:

- `evict_account(pubkey)` builds `InstructionUtils::evict_account_instruction(pubkey)` and submits it locally.
- `clone_account(AccountCloneRequest)` builds one or more account clone transactions and returns the last submitted signature.
- `clone_program(LoadedProgram)` builds one or more program-buffer/finalize/deploy transactions and returns the last submitted signature, or `Signature::default()` for retracted programs.

The trait and request types live in Chainlink. Preserve that boundary: Chainlink supplies validated clone requests; this crate materializes them.

### Constants and helper exports

- `MAX_INLINE_DATA_SIZE: usize = 63 * 1024` controls chunk size for inline data payloads.
- Internal `MAX_INLINE_TRANSACTION_SIZE` is `u16::MAX`; transactions larger than this fail preflight with `ClonerError::CloneTransactionTooLarge` before submission.
- `derive_buffer_pubkey(program_pubkey)` derives `Pubkey::find_program_address(&[b"buffer", program_pubkey], &validator_authority_id())` for program clone buffers.

### Error/diagnostic helper API

`account_cloner.rs` exports:

- `AccountClonerResult<T> = Result<T, AccountClonerError>`.
- `AccountClonerError::{RecvError, JoinError, CommittorServiceError}`.
- `map_committor_request_result(res, intent_committor)` which awaits a committor oneshot result and, for TableMania errors with a signature, fetches transaction logs and compute units through `BaseIntentCommittor::get_transaction` and `MagicblockRpcClient` helpers.

This helper is not part of the primary account clone path, but changing it can affect error observability for account/commit integration flows.

## Runtime flows

### Production startup wiring

```text
magicblock-api startup
  -> create TransactionSchedulerHandle and LatestBlock
  -> ChainlinkCloner::new(handle, latest_block)
  -> ProdInnerChainlink<ChainlinkCloner>::try_new_from_endpoints(...)
  -> Chainlink fetch/clone pipeline calls Cloner methods as needed
```

Replica mode disables Chainlink in `magicblock-api`; in that mode `ChainlinkCloner` is not constructed for active base-layer cloning.

### Regular account clone flow

1. Chainlink builds an `AccountCloneRequest` containing the target pubkey, resolved `AccountSharedData`, optional post-delegation actions, and delegation metadata.
2. `clone_account` reads the latest blockhash from `LatestBlock`.
3. If `request.account.data().len() <= MAX_INLINE_DATA_SIZE`, it first builds a small clone transaction with `CloneAccount`.
4. If the small transaction fits `MAX_INLINE_TRANSACTION_SIZE`, it is submitted through `send_tx` and the signature is returned.
5. If a nominally small account has post-delegation actions that push the transaction over the size limit, the cloner falls back to the chunked large-account path.
6. Large/chunked accounts use:
   1. `CloneAccountInit` with the first data chunk and full `AccountCloneFields`,
   2. one or more `CloneAccountContinue` instructions for remaining chunks,
   3. an additional final empty `CloneAccountContinue` plus post-delegation executor instruction when actions are present.
7. Every chunked transaction is checked with `ensure_transactions_fit` before submission.
8. Transactions are submitted sequentially. On any submission error, the cloner sends `CleanupPartialClone` and returns `FailedToCloneRegularAccount`.
9. The returned signature is the last successfully submitted transaction signature, or default only if no transaction was submitted.

Pitfalls:

- `AccountCloneFields` must preserve lamports, owner, executable, delegated, confined, and remote slot from the Chainlink-resolved account. Delegated accounts must continue to be presented locally with the correct owner/flags.
- Post-delegation actions are intentionally included both in clone instructions and in a sibling post-delegation action executor instruction. Keep this aligned with `programs/magicblock/src/clone_account/process_post_delegation_actions.rs`.
- Chunked clone cleanup is best-effort. Errors from `send_cleanup` are logged but do not replace the original clone error.

### Program clone flow

```text
LoadedProgram from Chainlink
  -> derive validator-owned buffer PDA ["buffer", program_id]
  -> clone ELF/program data into buffer (small or chunked)
  -> finalize into local program representation
  -> loader-specific deploy/authority instructions
  -> wait for one local slot before returning
```

1. Chainlink resolves a `LoadedProgram` and calls `clone_program`.
2. The cloner builds loader-specific transactions:
   - `RemoteProgramLoader::V1` uses `FinalizeV1ProgramFromBuffer` and creates local upgradeable-loader-style program/program-data accounts.
   - Other supported loaders use `FinalizeProgramFromBuffer`, `LoaderV4Instruction::Deploy`, and `SetProgramAuthority`.
3. `LoaderV4Status::Retracted` programs are skipped and return `Signature::default()`.
4. Program bytes are first cloned into a buffer PDA derived from validator authority and program id.
5. Small programs fit in one transaction containing buffer `CloneAccount` plus finalization instructions.
6. Large programs use `CloneAccountInit`, middle `CloneAccountContinue` chunks, and a final `CloneAccountContinue(is_last=true)` with finalization/deploy/authority instructions.
7. V1 and V4 finalization sequences temporarily disable and then re-enable executable checks via Magic Program instructions because finalization sets executable state.
8. On any transaction submission failure, cleanup targets the buffer PDA and returns `FailedToCloneProgram`.
9. After successful submission, `clone_program` waits until `LatestBlock` advances beyond the current slot before returning so the cloned program can be used.

Pitfalls:

- The buffer account is temporary and must stay deterministic; changing `derive_buffer_pubkey` affects cleanup, idempotency, and program clone compatibility.
- V1 programs are assumed immutable after local deployment; the code avoids a full upgrade flow and directly creates/updates program/program-data accounts.
- The one-slot wait is a readiness guarantee for program use. Removing it can introduce races where a just-cloned program is invoked before it is usable.

### Eviction flow

1. Chainlink requests eviction through the `Cloner` trait, typically as part of subscription/LRU or account lifecycle handling.
2. `evict_account` builds the Magic Program evict instruction and signs a local transaction with validator authority.
3. The transaction is submitted through the same scheduler path as clones.
4. Errors are wrapped as `ClonerError::FailedToEvictAccount`.

### Committor diagnostic mapping flow

1. A caller passes a committor oneshot receiver plus `Arc<impl BaseIntentCommittor>` to `map_committor_request_result`.
2. Send/receive failures become `AccountClonerError::CommittorServiceError` or `RecvError`.
3. Successful committor values are returned unchanged.
4. `CommittorServiceError::TableManiaError` with a signature triggers a transaction lookup for logs and compute units.
5. The final error string includes TableMania debug output plus available CUs/logs.

This helper performs remote/committor diagnostics and should not be inserted into hot clone submission paths without considering latency impact.

## Important internals and caveats

### Transaction signing and submission

All clone, evict, cleanup, finalize, and deploy transactions are signed with `magicblock_program::validator::validator_authority()`. `send_tx` captures `tx.signatures[0]`, encodes the transaction with `with_encoded`, and submits it through `TransactionSchedulerHandle::execute`.

Do not bypass the scheduler or write accounts directly from this crate. The local transaction path keeps Magic Program validation, account locking, ledger/status writes, and event emission consistent with normal validator execution.

### Size limits and chunking

`MAX_INLINE_DATA_SIZE` is an approximate payload chunk size, not a guarantee that every built transaction fits Solana's serialized transaction limit. The code separately checks serialized transaction size with `bincode::serialized_size` against `u16::MAX`. Post-delegation actions can make a small data payload too large, which is why the small-account path can fall back to chunking.

### Post-delegation actions

Delegation actions originate from DLP delegation records and are parsed/validated in Chainlink. This crate only transports and executes them as part of clone finalization. Actions attached to non-delegated or unresolved DLP-owned accounts should be rejected before they reach this crate; if that boundary changes, inspect `magicblock-chainlink/src/fetch_cloner/mod.rs` and its tests.

### Program loaders

The cloner treats `RemoteProgramLoader::V1` specially and sends all other non-retracted loaded programs through the V4 deployment path. Loader semantics are resolved in `magicblock-chainlink/src/remote_account_provider/program_account.rs`. If new loader variants or statuses are added, update both crates and this guide.

### README caveat

`magicblock-account-cloner/README.md` describes older conceptual components such as separate fetcher, updates, and dumper crates. The current repository places fetch/update/classification behavior primarily in `magicblock-chainlink`; this crate is the transaction-building and local materialization executor behind the Chainlink `Cloner` trait.

## Important invariants

1. This crate must not decide which accounts are safe to clone; Chainlink owns classification, delegation-record resolution, and request construction.
2. This crate must not bypass the local transaction scheduler or mutate `AccountsDb` directly.
3. `AccountCloneFields` must faithfully carry lamports, owner, executable, delegated, confined, and remote slot from the resolved account.
4. Large account clones must be completed with `CloneAccountContinue(is_last=true)` or cleaned up with `CleanupPartialClone` on failure.
5. Transaction size preflight must remain in place for chunked transactions, especially when post-delegation actions are present.
6. Post-delegation actions for delegated clones must remain synchronized between clone instructions and post-delegation action executor instructions.
7. Program clone buffer PDAs must remain deterministic and cleanup-compatible.
8. Program finalization must preserve loader-specific authority and deployment semantics.
9. Retracted programs must not be deployed locally as usable programs.
10. Program cloning must preserve the wait-until-next-slot readiness behavior unless a replacement readiness guarantee is implemented.
11. Any new work added to account/program clone paths must be bounded and avoid unnecessary allocations, serialization, logging, or scheduler submissions.

## Common change areas and what to inspect

### Changing regular account clone behavior

Inspect first:

- `magicblock-account-cloner/src/lib.rs` methods `clone_account`, `build_small_account_tx`, `build_large_account_txs`, `clone_fields`, and cleanup helpers;
- `magicblock-chainlink/src/cloner/mod.rs` for request/trait shape;
- `magicblock-chainlink/src/fetch_cloner/` for request construction and delegation-action validation;
- `programs/magicblock/src/clone_account/` for instruction processor expectations;
- unit tests in `magicblock-account-cloner/src/lib.rs`.

Risks:

- transaction-size regressions with large action payloads;
- partial clone state left behind after failures;
- delegated/confined/remote-slot flags diverging from Chainlink's resolved account state.

### Changing post-delegation action handling

Inspect first:

- `build_small_account_tx` and `build_large_account_txs`;
- `programs/magicblock/src/clone_account/process_post_delegation_actions.rs`;
- `magicblock-chainlink/src/fetch_cloner/delegation.rs` and action dependency validation in `fetch_cloner/mod.rs`;
- `test-integration/test-cloning/tests/10_post_delegation_token_transfer.rs`.

Risks:

- executing actions before the cloned account is fully materialized;
- allowing actions on non-delegated or unresolved accounts;
- producing transactions too large to execute.

### Changing program clone or loader behavior

Inspect first:

- `build_program_txs`, `build_v1_program_txs`, `build_v4_program_txs`, `build_program_txs_from_finalize`, and `build_large_program_txs`;
- `derive_buffer_pubkey` in `src/util.rs`;
- `magicblock-chainlink/src/remote_account_provider/program_account.rs`;
- Magic Program finalizers in `programs/magicblock/src/clone_account/`;
- loader API dependencies in `Cargo.toml`;
- `test-integration/test-cloning/tests/08_multi_program_cloning.rs`.

Risks:

- ABI/loader mismatches;
- executable checks left disabled after an error;
- program authority not matching the base-layer authority;
- program becoming visible before it is usable.

### Changing startup or Chainlink wiring

Inspect first:

- `magicblock-api/src/magic_validator.rs::init_chainlink`;
- type aliases `InnerChainlinkImpl` / `ChainlinkImpl` in `magicblock-api` and `magicblock-accounts`;
- `magicblock-chainlink` production aliases and disabled replication mode behavior.

Risks:

- constructing cloners in replica mode where Chainlink should be disabled;
- using a stale `LatestBlock` or wrong scheduler handle;
- changing account availability behavior for RPC and transaction submission.

### Changing diagnostic/error handling helpers

Inspect first:

- `magicblock-account-cloner/src/account_cloner.rs`;
- `magicblock-account-cloner/src/util.rs::get_tx_diagnostics`;
- `magicblock-committor-service` error types;
- `magicblock-rpc-client` transaction log/CU helpers;
- `magicblock-api/src/errors.rs` and `magicblock-accounts/src/errors.rs` conversions.

Risks:

- hiding commit/table errors needed for operator debugging;
- adding slow diagnostics to latency-sensitive clone paths;
- changing public error strings used by tests or logs.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-account-cloner` and, when request construction or account availability changes, `magicblock-chainlink`.
- Relevant integration suites: `test-cloning`; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Useful focused integration areas: `test-integration/test-cloning/tests/05_parallel-cloning.rs` for concurrent clone pressure, `test-integration/test-cloning/tests/08_multi_program_cloning.rs` for program cloning, and `test-integration/test-cloning/tests/10_post_delegation_token_transfer.rs` for post-delegation actions.
- Performance validation intent: account/program clone changes should report whether they add transactions, serialization, allocations, logging, scheduler waits, cleanup work, or remote diagnostics to clone hot paths.

## Adjacent implementation references

- `.agents/context/crates/magicblock-chainlink.md` — Chainlink-side account synchronization, request construction, delegation, subscription, and fetch/clone pipeline guidance.
- `magicblock-account-cloner/README.md` — high-level cloning overview; verify historical statements against current source.
- `magicblock-chainlink/src/cloner/mod.rs` — trait and request boundary implemented by this crate.
- `programs/magicblock/src/clone_account/` — Magic Program clone processors that consume instructions built here.
- `magicblock-magic-program-api/src/instruction.rs` — clone/finalize instruction wire types.
- `test-integration/test-cloning/` — integration tests for cloning behavior.
