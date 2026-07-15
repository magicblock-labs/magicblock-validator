# `magicblock-accounts`

## Purpose

`magicblock-accounts` is currently a legacy compatibility crate. It remains in
the workspace because older account/scheduled-commit types are still exported,
but no production validator service is wired through this crate.

Current source exports:

- `config::*`, including the local historical `LifecycleMode`;
- `errors`, including legacy account and scheduled-commit error types;
- `traits::*`, including the legacy `ScheduledCommitsProcessor` trait.

This crate does **not** own active account fetching/cloning, scheduled intent
execution, owner-program undelegation request handling, or committor delivery.

Current owners:

- account fetching/cloning: `magicblock-chainlink` and
  `magicblock-account-cloner`;
- scheduled intent acceptance, result handling, and pending-intent recovery:
  `magicblock-committor-service/src/service.rs` and
  `magicblock-committor-service/src/service/intent_client.rs`;
- owner-program `UndelegationRequest` observation and local
  `ScheduleCommitAndUndelegate` submission:
  `magicblock-services/src/undelegation_request_service.rs`;
- validator startup/shutdown wiring: `magicblock-api/src/magic_validator.rs`.

Do not add new runtime services here just because the crate name says
`accounts`. Prefer the current owner crate for the behavior being changed.

## Update Requirement

Update this document in the same change when:

- this crate gains or loses exports;
- the legacy `ScheduledCommitsProcessor` trait or account error surface changes;
- production wiring starts or stops depending on this crate;
- historical account-manager remnants are removed or intentionally revived.

For the general documentation-update rule, see
`.agents/memory/agent-memory-and-docs.md`.

## Where It Sits

| Path | Role |
|---|---|
| `magicblock-accounts/Cargo.toml` | Minimal dependencies for the legacy config, traits, and errors. |
| `magicblock-accounts/src/lib.rs` | Public surface: `config::*`, `errors`, and `traits::*`. |
| `magicblock-accounts/src/config.rs` | Historical `LifecycleMode` and `requires_ephemeral_validation` helper. Current validator lifecycle config comes from `magicblock-config`, not this local enum. |
| `magicblock-accounts/src/traits.rs` | Legacy `ScheduledCommitsProcessor` trait. There is no current production implementation in this crate. |
| `magicblock-accounts/src/errors.rs` | Legacy account and scheduled-commit error types. Check for real consumers before adding variants. |
| `magicblock-accounts/README.md` | Historical account-manager notes. Treat it as stale unless source confirms the described API still exists. |

The crate is still a workspace member and workspace dependency alias, but there
is no active production Rust consumer at the time of writing.

## Public API Shape

`magicblock-accounts/src/lib.rs` exports:

```rust
mod config;
pub mod errors;
mod traits;

pub use config::*;
pub use traits::*;
```

### Legacy `ScheduledCommitsProcessor`

`src/traits.rs` defines:

- `process`;
- `scheduled_commits_len`;
- `clear_scheduled_commits`;
- `stop`.

The active scheduled-intent service no longer consumes this trait. It lives in
`magicblock-committor-service` and accepts scheduled intents directly through an
internal ER intent client.

### Legacy Errors

`src/errors.rs` still exposes `AccountsError`,
`ScheduledCommitsProcessorError`, and result aliases. Several variants refer to
historical account-manager or scheduled-commit flows. Before reusing one for new
behavior, confirm there is an active caller that will receive and handle it.

## Important Caveats

### Do Not Revive Runtime Ownership Accidentally

This crate was previously used for account/scheduled-commit glue, but that
responsibility has moved. New services should be placed where the active runtime
boundary is:

- `magicblock-services` for small validator background services and adapters;
- `magicblock-committor-service` for base-layer intent execution and recovery;
- `magicblock-chainlink` / `magicblock-account-cloner` for account lifecycle and
  cloning;
- `magicblock-api` for startup/shutdown orchestration.

### Historical README/API Drift

The README describes older names such as `AccountsManager`,
`ExternalAccountsManager`, `BankAccountProvider`, `RemoteAccountCloner`,
`Transwise`, and `ensure_accounts`. These are not present in the current crate
source. Do not implement new features against those names without first checking
current Chainlink/cloner/API ownership and updating or removing stale docs.

### Local `LifecycleMode` Drift

`magicblock-accounts/src/config.rs` defines a `LifecycleMode` separate from
`magicblock-config::config::LifecycleMode`. Current repository usages of
validator lifecycle mode use `magicblock-config`, not this local type. Avoid
adding new configuration wiring through the local enum unless that duplication
is intentional and documented.

## Change Guidance

### Removing Legacy Surface

Inspect first:

- workspace and integration `Cargo.toml` files;
- `rg "magicblock_accounts|magicblock-accounts"`;
- `magicblock-api/src/errors.rs`;
- public downstream crates or examples if this crate is published.

Risks:

- deleting exported symbols can still be a public API break even when no
  current workspace consumer uses them;
- tests or external users may still compile against the legacy traits/errors.

### Adding New Account Lifecycle Behavior

Start with:

- `magicblock-chainlink`;
- `magicblock-account-cloner`;
- `magicblock-services`;
- `magicblock-api/src/magic_validator.rs`.

Do not put new account synchronization, DLP request scanning, or commit
scheduling loops in this crate without an explicit ownership decision.

## Tests And Validation

- Markdown-only guide changes: run `git diff --check`.
- Rust changes in this crate: run `cargo check -p magicblock-accounts`.
- If removing exports, also run the smallest affected consumer package checks
  found by `rg`.
- If moving behavior out of or into this crate, also run checks for the new
  owner crate and `magicblock-api`.

## Adjacent References

- `.agents/context/crates/magicblock-services.md` - background service adapters,
  including owner-program undelegation request observation.
- `.agents/context/crates/magicblock-committor-service.md` - scheduled intent
  acceptance, execution, result notification, and recovery.
- `.agents/context/crates/magicblock-chainlink.md` - delegation/account lifecycle
  coordination.
- `.agents/context/crates/magicblock-account-cloner.md` - base account/program
  cloning.
- `.agents/context/crates/magicblock-api.md` - validator startup/shutdown wiring.
