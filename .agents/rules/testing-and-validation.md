# Testing and Validation

This file defines how agents validate repository changes. Keep exact commands
here or in an executable skill rather than duplicating them across crate guides.

## Validation cadence

Use the smallest relevant check while a change is incomplete. Do not repeatedly
run workspace-wide format, lint, or test commands during exploration or midway
through a cross-crate integration where the entire workspace is not expected to
work.

Run the `mbv-check` full gate only when the user explicitly requests it or a
significant implementation milestone is complete and the whole workspace is
expected to pass:

```bash
make fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

If `nextest` is unavailable, use `cargo test` for the same scope. Do not run both
for equivalent coverage.

## Choosing a targeted check

Use the crate map and touched behavior to select the narrowest useful package or
test target:

- Runtime/execution changes: affected engine-facing package and processor/account tests.
- RPC changes: affected Aperture or API test target.
- Delegation/cloning changes: Chainlink package tests.
- Commit/undelegation changes: affected committor or service package tests.
- Config changes: config package tests and an applicable validator startup test.
- Task scheduler changes: task scheduler package tests.
- Ledger/recovery changes: affected ledger or recovery tests.

Useful forms:

```bash
cargo check -p <crate-name> --tests
cargo nextest run -p <crate-name>
cargo nextest run -p <crate-name> <test_name> --no-capture
```

Use `cargo test` when libtest-only flags such as `--exact` or
`--test-threads=1` are specifically required.

## Correctness and performance

Before reviewing or changing code, read `.agents/rules/invariants.md`. When a
change touches signer/authority checks, account synchronization, lock ordering,
or untrusted RPC/transaction input, run the focused test that exercises that
security boundary. An unvalidated security-relevant path must be reported as
residual risk.

The validator is performance-sensitive. For critical RPC, account sync,
scheduler/executor, storage, replication, or settlement paths, use the smallest
available test or measurement that can reveal latency, contention, allocation,
or I/O regressions. If no practical performance measurement is run, report that
the assessment was reasoned rather than measured.

For documentation-only changes, verify paths and links mentioned by `AGENTS.md`
and `.agents/` remain accurate.

## Reporting

Report:

- exact commands run and their results;
- checks skipped and why;
- performance-sensitive or security-relevant paths touched and the evidence used;
- any residual risk;
- whether agent documentation was updated for durable discoveries.
