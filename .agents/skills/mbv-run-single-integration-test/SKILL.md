---
name: mbv-run-single-integration-test
description: Runs a single magicblock-validator integration test with the correct validator setup. Brings up only the devnet and/or ephemeral validators a suite needs (mirroring CI), then runs one targeted test from the owning crate. Use when you need to run or debug one specific integration test or test function locally.
---

# Run Single Integration Test Skill

Run one integration test in the `magicblock-validator` repo with the correct
validator topology. Integration tests live under `test-integration/`, build SBF
programs, and spin up validators, so a single test still needs the right
validators running first.

Before running, the validator's own guidance in
`.agents/rules/testing-and-validation.md` is the source of truth for validation
and for the suite/setup-target table. This skill encodes the exact workflow so
you do not have to re-derive it.

## Overview

Isolating one integration test is a two-terminal workflow:
1. **Terminal A** — start only the validators the suite needs and leave them running.
2. **Terminal B** — run the single target test from the owning crate.
3. Stop the validators (Ctrl-C in terminal A) when done.

Assume all required tooling is installed and available (`cargo`, `cargo nextest`,
`make`). Do not check whether tools are installed.

## Quick Start

Prefer the repo's `make setup-<suite>-{devnet,ephem,both}` targets over
hand-built validator commands. They use the same suite-to-config mapping as CI
and avoid guessing which `.toml` files belong to the target test.

1. Identify the suite from the target test path (see the table below).
2. Build programs if the required `.so` files may be missing or stale:

   ```bash
   cd test-integration
   make programs
   ```

3. In terminal A, start only the validators the suite needs:

   ```bash
   cd test-integration
   make setup-<suite>-devnet
   # or
   make setup-<suite>-ephem
   # or
   make setup-<suite>-both
   ```

   The setup targets set `SETUP_ONLY` and then wait for Ctrl-C. Keep the
   terminal open while running the isolated test. List available targets with
   `make list`.

4. In terminal B, run only the target test from the owning crate.

   Prefer `cargo nextest` when available:

   ```bash
   cd test-integration
   RUST_LOG=info cargo nextest run -p <test-crate> --test <test_file> <test_name> --no-capture
   ```

   Use an exact nextest expression when needed:

   ```bash
   cd test-integration
   RUST_LOG=info cargo nextest run -p <test-crate> --test <test_file> -E 'test(<exact_test_name>)' --no-capture
   ```

   Use `cargo test` when you need libtest-only flags such as `--test-threads=1`
   or `--exact` (for example to mirror the integration runner's serial behavior):

   ```bash
   cd test-integration
   RUST_LOG=info cargo test -p <test-crate> --test <test_file> <test_name> -- --test-threads=1 --nocapture
   ```

5. Return to terminal A and press Ctrl-C. Do not leave local validators running
   after the test.

## Source Of Truth

Use `.agents/rules/testing-and-validation.md` as the primary reference for suite
names, setup targets, and test crates. For lower-level detail, the runner code is
authoritative:

- `test-integration/test-runner/bin/run_tests.rs` — suite name for `RUN_TESTS`,
  whether the suite needs devnet only, ephem only, or both, and which config
  files in `test-integration/configs/` the suite uses.
- `test-integration/test-runner/src/env_config.rs` — `RUN_TESTS` and
  `SETUP_ONLY` behavior.

`SETUP_ONLY` accepts `devnet`, `ephem`, or `both` (the `make setup-*` targets set
this for you).

## Suite → setup target → test crate

Use these known mappings first (full table in
`.agents/rules/testing-and-validation.md`):

| Area | Setup target(s) | Test crate/package |
|---|---|---|
| Chainlink/account sync | `make setup-chainlink-devnet` | `test-chainlink` |
| Cloning | `make setup-cloning-{devnet,ephem,both}` | `test-cloning` |
| Ledger restore/recovery | `make setup-restore-ledger-devnet` | `test-ledger-restore` |
| MagicBlock API | `make setup-magicblock-api-{devnet,ephem,both}` | `test-magicblock-api` |
| Pubsub | `make setup-pubsub-{devnet,ephem,both}` | `test-pubsub` |
| Config | `make setup-config-devnet` | `test-config` |
| Schedule intents | `make setup-schedule-intents-{devnet,ephem,both}` | `test-schedule-intent` |
| Task scheduler | `make setup-task-scheduler-devnet` | `test-task-scheduler` |
| TableMania | `make setup-table-mania-devnet` | `test-table-mania` |
| Committor | `make setup-committor-devnet` | `test-committor-service` |

### Concrete example — `task-scheduler` (devnet only)

Terminal A:

```bash
cd test-integration
make setup-task-scheduler-devnet
```

Terminal B:

```bash
cd test-integration
cargo test -p test-task-scheduler --test test_schedule_magic_cpi_crank test_crank_can_execute_program_that_cpis_into_magic -- --test-threads=1 --nocapture
```

Do not start an ephem validator for this suite.

### Concrete example — `schedulecommit` / schedule intents (devnet + ephem)

Terminal A:

```bash
cd test-integration
make setup-schedule-intents-both
```

Terminal B:

```bash
cd test-integration
cargo test -p test-schedule-intent --test 01_invocations test_schedule_commit_directly_with_single_ix -- --test-threads=1 --nocapture
```

## Manual Fallback

Use manual validator startup only when the user explicitly asks for raw
config-based setup instead of the `make setup-*` targets / `test-runner`.

When doing that:
1. Pick the matching `.devnet.toml` and/or `.ephem.toml` files from
   `test-integration/configs/` (confirm via `run_tests.rs`).
2. Start the chain validator with the repo's prewired script or equivalent
   `solana-test-validator` command.
3. Start the ephemeral validator with `cargo run -- <path-to-ephem-config>`.
4. Run the targeted test command from the owning crate.

Prefer the `make setup-*` targets unless there is a specific reason to bypass them.

## Troubleshooting

- If the test fails with a missing chain account or PDA after setup
  transactions, inspect the suite config first. A missing program in the devnet
  config can cause setup transactions to fail earlier than the observed
  assertion.
- For `task-scheduler`, the suite config must load every program the test
  touches on chain. If a new targeted test uses another program, update
  `test-integration/configs/schedule-task.devnet.toml` rather than guessing from
  a generic validator script.
- If the test binary builds but validator setup fails, rebuild artifacts with
  `make programs` in `test-integration`.
- If you are unsure which suite owns the test, derive it from the directory, then
  confirm it in `.agents/rules/testing-and-validation.md` and `run_tests.rs`
  before starting validators.

## Reporting

When finishing, report:
- the suite identified and the setup target used,
- the exact test command run and its pass/fail result,
- whether validators were stopped afterward,
- whether `.agents/` docs needed updates for any durable discovery.
