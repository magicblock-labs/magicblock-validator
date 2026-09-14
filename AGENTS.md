# Agent Guide

## Review

- Identify affected contracts and invariants. Existing gaps are not compliance;
  do not weaken signer checks, synchronization integrity, atomicity, or resource
  bounds to work around them. Security takes precedence over performance and convenience.
  Applicable violations block PR creation, approval, or a merge recommendation.
  End with `Invariants: clear` only when supported, otherwise state the applicable
  gap or violation and evidence.
- Preserve critical-path performance. Report unavoidable latency, throughput,
  contention, allocation, or I/O costs and mitigation; distinguish measured from
  reasoned assessments. Keep operator work off execution hot paths.

## Local ownership

- Leader orchestration: `bins/magicblock-validator`; follower lifecycle:
  `bins/magicblock-verifier`. Only the leader starts application services.
- Shared runtime image: `magicblock-runtime`; account synchronization/materialization:
  `magicblock-chainlink`; RPC: `magicblock-aperture`; settlement: committor crates;
  repeated task execution: `magicblock-task-scheduler`.
- Operator clients: `bins/magicblock` and `bins/magicblock-validator-tui`.
  Configuration and dependency resolution come from current manifests and source,
  not historical crate names. Execution, current account storage, and replication
  belong to Engine; inspect its resolved revision and local instructions for changes.
- Validator execution tests reuse Engine's `testkit` and v42 program. Do not add
  local test-program crates for that role.

## Validation

Use the smallest relevant check while working. Before pushing a code PR fix, run
exactly one relevant unit or integration test; broader validation belongs in CI. Exercise
changed authority/synchronization boundaries and report any unvalidated security path.

```sh
cargo check -p <package> --tests
cargo nextest run -p <package> <test_name> --no-capture
```

At an explicitly requested full gate or a completed significant milestone where
the workspace is expected to pass, run from the workspace root:

```sh
make ci-fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Use `make fmt` to apply formatting. If nextest is unavailable, use `cargo test` for
the same scope; use libtest when its specific flags are required. Do not run both
for equivalent coverage. Report exact commands, results, skipped checks, and residual
risk. For documentation-only changes, check links, paths, and instruction routing;
no Rust build is required. Test setup belongs to the owning target and current CI,
not a duplicated suite mapping in agent instructions.

## Pull requests and documentation

- Follow the current `.github/PULL_REQUEST_TEMPLATE.md` and CI conventions, including
  its dedicated `Closes #<issue>`. Do not hard-code template headings here.
- Do not put agent, assistant, model, or automation-tool names in GitHub-visible
  branches, commits, pull requests, or review replies.
- Keep docs out of code PRs. Queue durable discoveries, missing/stale guidance,
  and changed contracts in the handoff for the manually started weekly documentation task;
  explicit documentation/policy tasks may make documentation-only changes.
  This also applies to discoveries during read-only reviews and questions.
- Name the affected documentation and missing fact in the handoff, even if the fact
  already exists in code or an unrelated document. Mention docs only when changed
  or a concrete follow-up is needed.
- For release preparation, inspect `.github/workflows/prepare-release.yml` and
  current repository policy. Historical release prose is not publication authority.
