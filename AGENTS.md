# MBV repository

This workspace orchestrates validators. Execution, account storage, and
replication belong to Engine; inspect its resolved dependency revision before
changing that boundary.

## Boundaries

- `bins/magicblock-validator` owns leader orchestration;
  `bins/magicblock-verifier` owns follower lifecycle. Only the leader starts
  application services.
- `magicblock-runtime` owns the shared runtime image; `magicblock-chainlink`
  owns account synchronization/materialization; `magicblock-aperture` owns RPC;
  committor crates own settlement; `magicblock-task-scheduler` owns repeated tasks.
- Keep operator work in `bins/magicblock` and `bins/magicblock-validator-tui`,
  off execution hot paths.
- Reuse Engine's `testkit` and v42 program for validator execution tests;
  do not add local test-program crates.

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
no Rust build is needed. Report checks run and remaining gaps.
