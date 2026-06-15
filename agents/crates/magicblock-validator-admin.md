# `magicblock-validator-admin`

## Purpose

`magicblock-validator-admin` contains small operator/admin helpers that the validator uses to manage base-layer validator state. Its current implemented responsibility is claiming accrued Delegation Program validator fees from the validator fees vault.

High-level responsibilities:

- expose `claim_fees(url)` for one-shot validator fee claiming against a base-layer RPC endpoint;
- expose `ClaimFeesTask` for periodic fee-claim attempts owned by `magicblock-api::magic_validator::MagicValidator`;
- construct and sign the Delegation Program `validator_claim_fees` instruction using the validator authority from `magicblock-program`;
- avoid submitting fee-claim transactions when the fees vault balance is below the local minimum threshold;
- provide cooperative startup/shutdown behavior for the periodic Tokio task.

This crate sits on validator startup, background administration, and base-layer transaction paths. It is not part of per-transaction ER execution, but changes can affect operator cost collection, base-layer RPC load, startup latency, and graceful shutdown.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-validator-admin` change. In particular, update it for changes to:

- public exports in `src/lib.rs` or `src/claim_fees.rs`;
- `ClaimFeesTask` lifecycle, cancellation, tick scheduling, duplicate-start behavior, or shutdown timeout;
- `claim_fees` RPC commitment, fee-vault derivation, threshold, signer, payer, instruction construction, or error mapping;
- use of `magicblock-program::validator::validator_authority()` or Delegation Program APIs;
- startup/shutdown wiring in `magicblock-api`, especially `chain_operation.claim_fees_frequency` gating;
- configuration docs or defaults that affect fee claiming;
- tests, integration setup, or validation commands for validator fee claiming.

Because this crate sends operator/admin transactions to the base layer, also update this file when another crate changes validator authority initialization, Delegation Program fee-vault semantics, or how `MagicValidator` starts/stops admin background work.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-validator-admin/Cargo.toml` | Package metadata and dependencies on Delegation Program APIs, Magic Program validator authority helpers, Solana RPC/transaction crates, Tokio, cancellation tokens, and `magicblock-rpc-client` error types. |
| `magicblock-validator-admin/src/lib.rs` | Public crate surface. Currently exports only `pub mod claim_fees`. |
| `magicblock-validator-admin/src/claim_fees.rs` | Fee-claim implementation, `ClaimFeesTask`, periodic loop, minimum claim threshold, one-shot RPC transaction construction, and error mapping. |
| `magicblock-api/src/magic_validator.rs` | Main consumer. Owns `ClaimFeesTask`, calls one-shot `claim_fees` during on-chain setup, starts periodic claims for standalone validators, and stops the task during shutdown. |
| `magicblock-config/src/config/chain.rs` | Defines `ChainOperationConfig::claim_fees_frequency`; zero disables fee claiming and the default is 24 hours. |
| `config.example.toml` | Operator-facing `[chain-operation] claim-fees-frequency` example and environment variable name. |
| `test-integration/test-magicblock-api/tests/test_claim_fees.rs` | Integration coverage for instruction creation, `ClaimFeesTask` construction/defaults, fee-vault funding, direct fee-claim transaction, and RPC connectivity. |

Main consumers:

- `magicblock-api`, which owns runtime integration and lifecycle ordering;
- integration tests under `test-integration/test-magicblock-api`;
- future operator/admin code that needs validator-management helper transactions.

Important upstream dependencies:

- `magicblock-delegation-program-api` (imported as `dlp_api`) for `validator_claim_fees` and validator fees vault PDA derivation;
- `magicblock-program` for the validator authority keypair used to sign fee-claim transactions;
- `solana_rpc_client::nonblocking::rpc_client::RpcClient` for balance, blockhash, and send/confirm calls;
- `magicblock-rpc-client` for the shared `MagicBlockRpcClientError` type used by this crate's public result.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exposes:

- `pub mod claim_fees`.

All public APIs currently live under `magicblock_validator_admin::claim_fees`.

### `ClaimFeesTask`

`ClaimFeesTask` is a small Tokio task handle plus cancellation token:

- `ClaimFeesTask::new()` and `Default` create an idle task with `handle: None`;
- `start(tick_period, url)` spawns `run_claim_fees_loop` unless the task has already been started;
- `stop().await` cancels the token, waits up to two seconds for the JoinHandle, and logs if the task does not stop within the grace period;
- `handle` is public and currently used by tests to verify idle construction; the cancellation token is private.

`start` schedules the first claim for `Instant::now() + tick_period`, not immediately. `MagicValidator` separately performs a one-shot startup claim during on-chain setup when fee claiming is enabled.

### `claim_fees`

`claim_fees(url: String) -> Result<(), MagicBlockRpcClientError>` performs one fee-claim attempt:

1. Creates a Solana nonblocking `RpcClient` with `CommitmentConfig::confirmed()`.
2. Loads the validator authority keypair via `magicblock_program::validator::validator_authority()`.
3. Derives the validator fees vault PDA with `dlp_api::pda::validator_fees_vault_pda_from_validator`.
4. Reads the vault balance.
5. Returns `Ok(())` without sending a transaction if the balance is `<= MIN_CLAIMABLE_LAMPORTS` (`100_000_000`).
6. Builds `dlp_api::instruction_builder::validator_claim_fees(validator, None)`.
7. Fetches a latest blockhash.
8. Signs a transaction with the validator as payer and signer.
9. Sends and confirms the transaction.

Error mapping is intentionally narrow and currently wraps Solana RPC failures as `RpcClientError`, `GetLatestBlockhash`, or `SendTransaction` variants from `magicblock-rpc-client`.

## Runtime flows

### Startup one-shot claim

```text
MagicValidator::spawn_primary_onchain_setup
  -> ensure validator is funded on the base chain
  -> ensure Magic fee vault exists
  -> if chain_operation.claim_fees_frequency is non-zero:
       claim_fees(rpc_url).await
       log but do not abort on failure
  -> optionally register validator on-chain
```

The startup claim runs only inside the primary on-chain setup path. Failure is logged and does not stop startup, unlike the funding/vault setup failures immediately before it.

### Periodic background claim

```text
MagicValidator::start
  -> after ledger replay/reset and primary/standalone mode setup
  -> if is_standalone && chain_operation.claim_fees_frequency is non-zero:
       claim_fees_task.start(frequency, config.rpc_url())

ClaimFeesTask loop
  -> wait one full tick period before first interval tick
  -> call claim_fees(url.clone()) on each tick
  -> log errors and continue
  -> exit when cancellation token is cancelled
```

The periodic task uses the validator's configured RPC URL. Do not move it onto transaction execution or scheduler threads.

### Shutdown

```text
MagicValidator::stop
  -> stop scheduled-commit processor
  -> stop committor service
  -> claim_fees_task.stop().await
  -> join RPC thread and remaining validator services
```

`ClaimFeesTask::stop` is cooperative. It cancels the loop and waits briefly for the task, but it does not abort the Tokio task if the underlying RPC call is still blocked. Changes that increase claim RPC duration can therefore increase shutdown latency up to the grace-period behavior and leave the spawned task to finish later.

## Important internals and caveats

### Minimum claim threshold

`MIN_CLAIMABLE_LAMPORTS` is `100_000_000`. Balances at or below the threshold are skipped to avoid spending transaction fees on small claims. This threshold is crate-local and is not currently configurable.

### Validator authority and fee vault

The fee-claim transaction is signed by `validator_authority()` from `magicblock-program`, and the validator pubkey is also used as the transaction payer. The fees vault PDA must be derived from the same validator pubkey. If validator identity initialization changes, verify this helper still signs with the intended base-layer authority.

### RPC client choice

This crate currently constructs a raw Solana nonblocking `RpcClient` for one-shot fee claims instead of using the `MagicblockRpcClient` wrapper. It still reuses `MagicBlockRpcClientError` for public error compatibility with other base-layer helper code. If confirmation behavior, retry policy, or metrics are needed here, inspect `agents/crates/magicblock-rpc-client.md` before changing the client type.

### Configuration gating lives outside this crate

`ClaimFeesTask::start` assumes the caller already chose a non-zero tick period. The enable/disable policy lives in `magicblock-api` and `magicblock-config` via `[chain-operation] claim-fees-frequency`; zero disables both startup and periodic fee claiming where checked.

## Important invariants

1. Fee claims must use the validator authority keypair that matches the validator fees vault PDA.
2. The validator pubkey must remain the payer and signer for `validator_claim_fees` transactions unless Delegation Program requirements change.
3. The crate must not send a claim transaction when the vault balance is at or below `MIN_CLAIMABLE_LAMPORTS`.
4. Startup fee-claim failures must remain observable through logs; do not silently swallow errors.
5. Periodic fee-claim failures must not crash the validator or terminate the loop unless the task is explicitly cancelled.
6. `ClaimFeesTask::start` must not spawn multiple loops for the same task instance.
7. `ClaimFeesTask::stop` must cancel and join cooperatively so validator shutdown can make progress.
8. Do not perform fee-claim RPC work on scheduler/executor hot paths.
9. Keep configuration semantics aligned across `magicblock-config`, `config.example.toml`, and `magicblock-api` lifecycle wiring.

## Common change areas and what to inspect

### Changing fee-claim transaction semantics

Start with:

- `magicblock-validator-admin/src/claim_fees.rs` (`claim_fees`, `MIN_CLAIMABLE_LAMPORTS`);
- Delegation Program API helpers used through `dlp_api::instruction_builder::validator_claim_fees` and `dlp_api::pda::validator_fees_vault_pda_from_validator`;
- `magicblock-program` validator authority helpers;
- `test-integration/test-magicblock-api/tests/test_claim_fees.rs`.

Check signer/payer requirements, vault PDA derivation, commitment level, blockhash freshness, and whether error mapping still tells operators what failed.

### Changing periodic scheduling or shutdown

Start with:

- `ClaimFeesTask::start`, `run_claim_fees_loop`, and `ClaimFeesTask::stop`;
- `magicblock-api/src/magic_validator.rs` fields, startup, and shutdown sections using `claim_fees_task`;
- `magicblock-config/src/config/chain.rs` and `config.example.toml` for frequency semantics.

Preserve duplicate-start protection, cancellation responsiveness, and the fact that the periodic loop waits one full period before its first tick.

### Adding new admin helpers

Keep this crate focused on validator/operator management helpers. New helpers should have explicit lifecycle owners in `magicblock-api` or operator tooling, clear signer requirements, bounded RPC behavior, and targeted validation. Avoid embedding general RPC client wrappers or core protocol execution logic here.

## Tests and validation

For documentation-only changes, verify the new guide path and cross-references:

```bash
ls agents/crates/magicblock-validator-admin.md
rg "magicblock-validator-admin.md" AGENTS.md agents/04_crate-map.md
```

For Rust changes in this crate, run at least:

```bash
cargo fmt
cargo nextest run -p magicblock-validator-admin
```

For lifecycle or config integration changes, also run relevant API/config tests:

```bash
cargo nextest run -p magicblock-api
cargo nextest run -p magicblock-config claim_fees
```

For end-to-end fee-claim behavior, use the MagicBlock API integration suite or the specific test when the devnet validator harness is available:

```bash
cd test-integration
make test-magicblock-api
# or, with the required validators already started:
RUST_LOG=info cargo test -p test-magicblock-api --test test_claim_fees -- --test-threads=1 --nocapture
```

Broader baseline before handing off code changes remains:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance-sensitive risk to report: this crate can add base-layer RPC load and startup/shutdown latency. If fee-claim frequency, retry behavior, confirmation behavior, or task cancellation changes, state whether RPC-load and shutdown-latency risk was measured or only reasoned about.

## Related docs

- `agents/00_overview.md` — validator runtime model and operator/admin flows.
- `agents/03_architecture.md` — process/service orchestration and startup/shutdown boundaries.
- `agents/04_crate-map.md` — repository crate ownership map.
- `agents/05_testing-and-validation.md` — repository validation workflow.
- `agents/crates/magicblock-api.md` — `MagicValidator` startup/shutdown owner for this crate's task.
- `agents/crates/magicblock-config.md` — `[chain-operation]` config model and env/TOML behavior.
- `agents/crates/magicblock-rpc-client.md` — shared base-layer RPC wrapper and error type used by related admin/settlement helpers.
- `config.example.toml` — operator-facing `claim-fees-frequency` example.
