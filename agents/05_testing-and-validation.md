# Testing and Validation

This file tells agents how to validate changes. Keep it focused on commands and workflow. If validation behavior changes, update this file in the same change.

## Baseline rule

Every code change must be validated. Use the `rs-check` skill for Rust changes once it is available in the agent environment. The intent of that skill is to run the standard Rust quality gate and help fix any failures.

The validator is performance-sensitive. When a change touches critical RPC, account synchronization, scheduler/executor, AccountsDb/ledger, replication, or committor paths, validation should include the smallest available test or measurement that can reveal latency, throughput, contention, allocation, or I/O regressions. If no practical performance validation is run, say so and explain the residual risk.

Until or unless the skill provides a more specific command set, treat the required baseline as:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

For small or targeted changes, run the smallest relevant test first, then run the broader checks before handing off if time allows.

For documentation-only changes, at minimum verify the changed files are in the right location and that links/filenames mentioned in `AGENTS.md` and `agents/` stay in sync.

## Choosing what to run

Use the crate map and touched files to pick tests:

- Runtime/execution changes: test the affected crate and relevant processor/account tests.
- RPC changes: test the affected aperture/API area and any matching integration suite.
- Delegation/cloning changes: run chainlink/cloning integration tests.
- Commit/undelegation changes: run committor, schedule intent, and related integration tests.
- Config changes: run config tests and at least one validator startup path.
- Task scheduler changes: run task scheduler tests.
- Ledger/recovery changes: run ledger restore tests.

Always report exactly which commands were run and whether they passed. Also report any performance-sensitive paths touched, what performance validation was performed, and any unavoidable performance tradeoff or unmeasured risk.

## Workspace checks

Common root-level commands:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Useful targeted forms:

```bash
cargo test -p <crate-name>
cargo nextest run -p <crate-name>
cargo test -p <crate-name> <test_name> -- --nocapture
cargo nextest run -p <crate-name> <test_name> --no-capture
```

Use `cargo test` when you need libtest flags such as `--exact`, `--test-threads=1`, or when matching the integration runner's behavior exactly.

## Integration test structure

Integration tests live under `test-integration/`. The integration workspace has its own `Cargo.toml`, test crates, test programs, and Makefile.

Run all integration tests:

```bash
cd test-integration
make test
```

Run one integration suite through the runner:

```bash
cd test-integration
make test-chainlink
make test-cloning
make test-restore-ledger
make test-magicblock-api
make test-table-mania
make test-committor
make test-pubsub
make test-config
make test-schedule-intents
make test-task-scheduler
```

The Makefile uses `RUN_TESTS=<suite>` internally. You can also invoke it directly:

```bash
cd test-integration
RUN_TESTS=cloning make test
RUN_TESTS=committor_intent_executor make test
```

The integration runner builds required SBF programs via `make programs` as dependencies of `make test`.

## Isolating one integration test

For fast debugging, start only the validators needed by a suite in one terminal, then run the desired Rust test directly in another terminal.

### 1. Start validators and leave them running

From terminal A:

```bash
cd test-integration
make setup-<suite>-devnet
# or
make setup-<suite>-ephem
# or
make setup-<suite>-both
```

Examples:

```bash
cd test-integration
make setup-cloning-both
make setup-chainlink-devnet
make setup-magicblock-api-both
make setup-pubsub-both
make setup-schedule-intents-both
make setup-task-scheduler-devnet
```

The setup targets set `SETUP_ONLY` and then wait for Ctrl-C. Keep that terminal open while running the isolated test.

Available setup targets are listed by:

```bash
cd test-integration
make list
```

### 2. Run the specific test directly

From terminal B, run the test in the relevant integration crate.

With `cargo test`:

```bash
cd test-integration
RUST_LOG=info cargo test -p <test-crate> --test <test_file> <test_name> -- --test-threads=1 --nocapture
```

Example:

```bash
cd test-integration
RUST_LOG=info cargo test -p test-cloning --test 03_get_multiple_accounts <test_name> -- --test-threads=1 --nocapture
```

Use `--exact` if the filter accidentally matches multiple tests:

```bash
cd test-integration
RUST_LOG=info cargo test -p <test-crate> --test <test_file> <exact_test_name> -- --exact --test-threads=1 --nocapture
```

With `cargo nextest`:

```bash
cd test-integration
RUST_LOG=info cargo nextest run -p <test-crate> --test <test_file> <test_name> --no-capture
```

Or with an exact nextest expression:

```bash
cd test-integration
RUST_LOG=info cargo nextest run -p <test-crate> --test <test_file> -E 'test(<exact_test_name>)' --no-capture
```

### 3. Stop validators

Return to terminal A and press Ctrl-C. Do not leave local validators running after the test.

## Suite names and setup targets

Common suite names used by the Makefile:

| Area | Runner target | Setup target(s) | Test crate/package |
|---|---|---|---|
| Chainlink/account sync | `make test-chainlink` | `make setup-chainlink-devnet` | `test-chainlink` |
| Cloning | `make test-cloning` | `make setup-cloning-devnet`, `make setup-cloning-ephem`, `make setup-cloning-both` | `test-cloning` |
| Ledger restore/recovery | `make test-restore-ledger` | `make setup-restore-ledger-devnet` | `test-ledger-restore` |
| MagicBlock API | `make test-magicblock-api` | `make setup-magicblock-api-devnet`, `make setup-magicblock-api-ephem`, `make setup-magicblock-api-both` | `test-magicblock-api` |
| Pubsub | `make test-pubsub` | `make setup-pubsub-devnet`, `make setup-pubsub-ephem`, `make setup-pubsub-both` | `test-pubsub` |
| Config | `make test-config` | `make setup-config-devnet` | `test-config` |
| Schedule intents | `make test-schedule-intents` | `make setup-schedule-intents-devnet`, `make setup-schedule-intents-ephem`, `make setup-schedule-intents-both` | `test-schedule-intent` |
| Task scheduler | `make test-task-scheduler` | `make setup-task-scheduler-devnet` | `test-task-scheduler` |
| TableMania | `make test-table-mania` | `make setup-table-mania-devnet` | `test-table-mania` |
| Committor | `make test-committor` and narrower committor targets | `make setup-committor-devnet` | `test-committor-service` |

Committor has narrower Make targets for long suites:

```bash
make test-committor-ix-singles
make test-committor-preparators
make test-committor-ix-order
make test-committor-ix-multi
make test-committor-bundles
make test-committor-intent-bundles
make test-committor-bundles-heavy
make test-committor-commitfinalize
make test-committor-intent-executor
make test-committor-intent-executor-recovery
```

## Reporting validation

When finishing a task, include:

- commands run,
- pass/fail result,
- if skipped, why it was skipped,
- any remaining risk, especially for integration tests that require validators or long-running suites,
- any performance-sensitive paths touched and whether performance regression risk was measured, reasoned about, or left unmeasured.
