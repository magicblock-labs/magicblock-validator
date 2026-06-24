# Testing and Validation

This file tells agents how to validate changes. Keep it focused on commands and workflow. If validation behavior changes, update this file in the same change.

## Baseline rule

Every code change must be validated. Use the repository-local `mbv-check` skill for Rust changes once it is available in the agent environment. The intent of that skill is to run this validator's standard Rust quality gate and help fix any failures.

Crate guides should name relevant packages/suites and validation intent, but exact command syntax belongs here or in executable skills.

The validator is performance-sensitive. When a change touches critical RPC, account synchronization, scheduler/executor, AccountsDb/ledger, replication, or committor paths, validation should include the smallest available test or measurement that can reveal latency, throughput, contention, allocation, or I/O regressions. If no practical performance validation is run, say so and explain the residual risk.

For security and protocol invariants, read `.agents/rules/validator-goals.md` and `.agents/specs/validator-specification.md`; this file only defines how to validate changes against those invariants. When a change touches signer/authority checks, account-sync correctness, lock acquisition/ordering, or any path driven by untrusted RPC/transaction input, add or run the test that exercises the security-relevant behavior (for example concurrency/race tests, delegation/sync ordering tests, or auth-rejection tests). If you cannot validate a security-relevant path, say so and call out the residual risk explicitly — do not treat it as low priority.

Until or unless the skill provides a more specific command set, treat the required baseline as:

```bash
make fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Run tests with `cargo nextest`. If `nextest` is not available in the environment, fall back to `cargo test` for the same scope (for example `cargo test --workspace`). This is the single source of truth for that choice; other sections assume it.

For small or targeted changes, run the smallest relevant test first, then run the broader checks before handing off if time allows.

For documentation-only changes, at minimum verify the changed files are in the right location and that links/filenames mentioned in `AGENTS.md` and `.agents/` stay in sync.

When you discover a new reliable way to test, debug, benchmark, or validate the codebase, update this file or the relevant crate-specific guide before finishing. If the approach is specific to one crate, prefer `.agents/context/crates/<crate>.md` and link from broader docs only when needed.

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
make fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Useful targeted forms:

```bash
cargo nextest run -p <crate-name>
cargo nextest run -p <crate-name> <test_name> --no-capture
```

`cargo test` and `cargo nextest` both run tests; use one of them rather than both for the same scope. Prefer `cargo nextest` when available. Use `cargo test` when `nextest` is unavailable, when you need libtest flags such as `--exact` or `--test-threads=1`, or when matching the integration runner's behavior exactly.

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

Prefer `cargo nextest` when available:

```bash
cd test-integration
RUST_LOG=info cargo nextest run -p <test-crate> <test_name> --no-capture
```

Or with an exact nextest expression:

```bash
cd test-integration
RUST_LOG=info cargo nextest run -p <test-crate> -E 'test(<exact_test_name>)' --no-capture
```

Use `cargo test` instead when you need libtest-only flags such as `--test-threads=1` or `--exact`:

```bash
cd test-integration
RUST_LOG=info cargo test -p <test-crate> --test <test_file> <test_name> -- --test-threads=1 --nocapture
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
- any performance-sensitive paths touched and whether performance regression risk was measured, reasoned about, or left unmeasured,
- any security-relevant paths touched (signer/authority enforcement, base-layer sync, locking/concurrency, untrusted-input handling), confirmation that no security property was weakened, and how that was checked or what residual risk remains,
- whether agent documentation was updated for any durable discovery, or why no update was needed.
