---
name: run-targeted-integration-test
description: Run a single integration test in the magicblock-validator repo with the correct validator setup. Use when Codex needs to run one specific integration test or test function locally, bring up only the devnet and/or ephemeral validators required for a suite, mirror the suite setup used by CI, or determine which config files and commands match a target test path.
---

# Run Targeted Integration Test

## Quick Start

Prefer the repo's `test-runner` setup-only mode over hand-built validator commands. It uses the same suite-to-config mapping as CI and avoids guessing which `.toml` files belong to the target test.

Use this workflow:

1. Identify the suite from the target test path.
2. Read `test-integration/test-runner/bin/run_tests.rs` to find the suite's devnet and ephem configs.
3. Build programs if the required `.so` files may be missing or stale:

```bash
cd test-integration
make programs
```

4. Start only the validators the suite needs:

```bash
cd test-integration
env RUN_TESTS=<suite> SETUP_ONLY=devnet cargo run --package test-runner --bin run-tests
```

```bash
cd test-integration
env RUN_TESTS=<suite> SETUP_ONLY=ephem cargo run --package test-runner --bin run-tests
```

```bash
cd test-integration
env RUN_TESTS=<suite> SETUP_ONLY=both cargo run --package test-runner --bin run-tests
```

5. In another shell, run only the target test from the owning crate:

```bash
cargo test --test <test-file-stem> <test-name> --profile test -- --test-threads=1 --nocapture
```

6. Stop the validator processes with `Ctrl-C` after the test finishes.

Always use `--test-threads=1` for targeted integration runs unless there is a clear reason not to.

## Source Of Truth

Use `test-integration/test-runner/bin/run_tests.rs` as the source of truth for:

- The suite name to pass in `RUN_TESTS`
- Whether the suite needs devnet only, ephem only, or both
- Which config files in `test-integration/configs/` the suite uses

Read `test-integration/test-runner/src/env_config.rs` when you need to confirm `RUN_TESTS` and `SETUP_ONLY` behavior.

`SETUP_ONLY` accepts:

- `devnet`
- `ephem`
- `both`

## Common Mappings

Use these known mappings first.

### `task-scheduler`

Path:

- `test-integration/test-task-scheduler`

Topology:

- devnet only

Config:

- `test-integration/configs/schedule-task.devnet.toml`

Setup command:

```bash
cd test-integration
env RUN_TESTS=task-scheduler SETUP_ONLY=devnet cargo run --package test-runner --bin run-tests
```

Concrete example:

```bash
cd test-integration/test-task-scheduler
cargo test --test test_schedule_magic_cpi_crank test_crank_can_execute_program_that_cpis_into_magic --profile test -- --test-threads=1 --nocapture
```

Do not start an ephem validator for this suite.

### `schedulecommit`

Paths:

- `test-integration/schedulecommit/test-security`
- `test-integration/schedulecommit/test-scenarios`

Topology:

- devnet plus ephem

Configs:

- devnet: `test-integration/configs/schedulecommit-conf.devnet.toml`
- ephem: `test-integration/configs/schedulecommit-conf-fees.ephem.toml`

Setup command:

```bash
cd test-integration
env RUN_TESTS=schedulecommit SETUP_ONLY=both cargo run --package test-runner --bin run-tests
```

Concrete example:

```bash
cd test-integration/schedulecommit/test-security
cargo test --test 01_invocations test_schedule_commit_directly_with_single_ix --profile test -- --test-threads=1 --nocapture
```

## Manual Fallback

Use manual validator startup only when the user explicitly asks for raw config-based setup instead of `test-runner`.

When doing that:

1. Pick the matching `.devnet.toml` and `.ephem.toml` files from `test-integration/configs/`.
2. Start the chain validator with the repo's prewired script or equivalent `solana-test-validator` command.
3. Start the ephemeral validator with:

```bash
cargo run -- <path-to-ephem-config>
```

4. Run the targeted `cargo test` command from the owning crate.

Prefer `test-runner` unless there is a specific reason to bypass it.

## Troubleshooting

If the test fails with a missing chain account or missing PDA after setup transactions, inspect the suite config first. A missing program in the devnet config can cause setup transactions to fail earlier than the observed assertion.

For `task-scheduler`, remember that the suite config must load every program the test touches on chain. If a new targeted test uses another program, update `test-integration/configs/schedule-task.devnet.toml` rather than guessing from a generic validator script.

If the test binary builds but the validator setup fails, rebuild the artifacts with `make programs` in `test-integration`.

If you are unsure which suite owns the test, derive it from the directory and then confirm it in `run_tests.rs` before starting validators.
