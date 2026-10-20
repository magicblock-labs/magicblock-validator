# MBV repository

This workspace owns validator orchestration, execution, account storage,
replication, and application services.

## Boundaries

- `magicblock-validator` owns process startup and shutdown;
  `magicblock-api` owns validator lifecycle and service wiring.
- `magicblock-processor` owns transaction execution; `magicblock-accounts-db`
  owns account storage; `magicblock-ledger` owns ledger persistence;
  `magicblock-replicator` owns replication.
- `magicblock-chainlink` owns account synchronization/materialization;
  `magicblock-aperture` owns RPC; committor crates own settlement;
  `magicblock-task-scheduler` owns repeated tasks.
- Keep operator tooling in `tools/`, off execution hot paths.
- Reuse `test-kit` and the existing `programs/guinea` program for execution
  tests; do not add local test-program crates.

## Review

Never weaken signer checks, synchronization integrity, atomicity, or resource
bounds to work around existing gaps. Applicable violations block PR creation,
approval, and merge recommendations. Report concrete gaps and unvalidated
security paths, plus critical-path costs; distinguish measured from reasoned
performance claims.

## Validation

Use `make fmt` for formatting and `cargo check -p <package> --tests` for the
affected package. Before pushing a code fix, run exactly one relevant test:
`cargo nextest run -p <package> <test_name> --no-capture`. Use `cargo test` if
nextest is unavailable or libtest-specific flags are needed.

For an explicitly requested full gate or a completed significant milestone:

```sh
make ci-fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

For documentation-only changes, check links, paths, and `git diff --check`;
no Rust build is needed. Report checks run and remaining gaps in the completion
report to the user. Do not list routine check results in PR descriptions.
