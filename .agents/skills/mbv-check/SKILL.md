---
name: mbv-check
description: Formats, lints, and tests the magicblock-validator Rust workspace using its exact cargo/make commands. Runs nightly rustfmt, workspace clippy, and nextest with optional error fixing. Use for Rust code quality checks and testing in this repository.
---

# MagicBlock Validator Check Skill

Automated formatting, linting, and testing tailored to the `magicblock-validator`
workspace. This repository is **Rust only**; there are no JS/TS fallbacks.

Before running, the validator's own guidance in `.agents/rules/testing-and-validation.md`
is the source of truth for validation. This skill encodes the exact commands that
repository uses so you do not have to re-derive them.

## Overview

Runs three sequential checks:
1. **Format** — nightly rustfmt with the repo's strict config
2. **Lint** — workspace clippy with warnings denied
3. **Test** — `cargo nextest` across the workspace

## Usage

When this skill loads, immediately run format, lint, and test with the default
mode (`fix-lint`). Do not ask the user for input.

Assume all required tooling is installed and available, including `cargo`, the
nightly toolchain, `cargo fmt`, `cargo clippy`, and `cargo nextest`. Do not check
whether tools are installed and do not hedge about availability.

All commands run from the workspace root unless explicitly noted (integration
tests run from `test-integration/`).

### Options (optional, user may specify)

- **fix-lint**: Fix clippy findings automatically (default mode)
- **fix-tests**: Attempt to fix test failures automatically
- **no-fix**: Don't fix anything, only report

If no option is specified, use `fix-lint`.

## Workflow

### 1. Format

This repo formats with **nightly** rustfmt and a stricter config
(`rustfmt-nightly.toml`: `imports_granularity = "Crate"`,
`group_imports = "StdExternalCrate"`). Use the repo target:

```bash
make fmt
# equivalent to:
cargo +nightly fmt -- --config-path rustfmt-nightly.toml
```

To only verify without writing (matches CI):

```bash
make ci-fmt
# equivalent to:
cargo +nightly fmt --check -- --config-path rustfmt-nightly.toml
```

Format always runs first regardless of options.

### 2. Lint

```bash
make lint
# equivalent to:
cargo clippy --all-targets -- -D warnings
```

Prefer the full-workspace form for broader changes (matches the documented
baseline in `.agents/rules/testing-and-validation.md`):

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

- With `fix-lint` (default): `cargo clippy --fix --allow-dirty --allow-staged --workspace --all-targets`, then re-run the deny-warnings check above to confirm it is clean.
- With `no-fix`: report findings only.

### 3. Test

Use `cargo nextest`. The repo's `make test` runs the workspace suite **and** the
integration suite; the integration suite is slow and needs SBF programs/validators,
so for a normal quality gate run the **workspace unit/integration-crate tests only**:

```bash
cargo nextest run --workspace
```

Targeted forms:

```bash
cargo nextest run -p <crate-name>
cargo nextest run -p <crate-name> <test_name> --no-capture
```

If `nextest` is genuinely unavailable, fall back to `cargo test --workspace`.

- With `fix-tests`: attempt focused fixes for real failures; do not mask root causes.
- With `no-fix`: report failures only.

## Choosing what to run (targeted changes)

For small changes, run the smallest relevant crate test first, then widen. Map
touched areas to crates/suites (see `.agents/context/crate-map.md` and
`.agents/rules/testing-and-validation.md`):

- Runtime/execution: `magicblock-processor`, account/runtime tests
- RPC: `magicblock-aperture`, `magicblock-api`
- Delegation/cloning: `magicblock-chainlink`, `magicblock-account-cloner`
- Commit/undelegation: `magicblock-committor-service`, schedule-intent tests
- Config: `magicblock-config`
- Task scheduler: `magicblock-task-scheduler`
- Ledger/recovery: `magicblock-ledger`

## Integration tests (only when relevant / requested)

Integration tests live under `test-integration/`, have their own workspace, build
SBF programs, and spin up validators. They are NOT part of the default gate. Run
them when a change touches an integration-covered path or the user asks.

```bash
cd test-integration
make test                 # all integration suites (builds programs first)
make test-chainlink
make test-cloning
make test-committor
make test-magicblock-api
make test-pubsub
make test-schedule-intents
make test-task-scheduler
make test-table-mania
make test-restore-ledger
make test-config
```

To isolate a single integration test, start validators in one terminal
(`make setup-<suite>-{devnet,ephem,both}`), then run the test directly in another
(`cargo nextest run -p <test-crate> --test <file> <name> --no-capture`). Stop the
validators with Ctrl-C afterward. See `.agents/rules/testing-and-validation.md` for the
full suite/setup-target table and details.

## Notes & guardrails

- Treat all commands here as ready to run locally; no install/availability checks.
- Format halts nothing; lint and test failures are logged but don't abort the skill.
- This validator is **performance-sensitive and security-critical**. When a change
  touches critical RPC, account sync, scheduler/executor, AccountsDb/ledger,
  replication, committor, signer/authority, or locking/concurrency paths, also run
  the smallest test that exercises that behavior and report any unmeasured
  perf/security risk (per `.agents/rules/testing-and-validation.md`).
- Use `fix-tests` carefully; automatic fixes may not address root causes.

## Reporting

When finishing, report:
- exact commands run and pass/fail for each,
- anything skipped and why (especially integration suites),
- any performance-sensitive or security-relevant paths touched and how risk was
  checked or what residual risk remains,
- whether `.agents/` docs needed updates for any durable discovery.
