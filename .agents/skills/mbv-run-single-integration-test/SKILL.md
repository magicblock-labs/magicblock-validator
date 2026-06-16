---
name: mbv-run-single-integration-test
description: Runs a single magicblock-validator integration test with the correct validator setup. Brings up only the devnet and/or ephemeral validators a suite needs (mirroring CI), then runs one targeted test from the owning crate. Use when you need to run or debug one specific integration test or test function locally.
---

# Run Single Integration Test Skill

Run one integration test in the `magicblock-validator` repo with the correct
validator topology. Integration tests live under `test-integration/`, build SBF
programs, and spin up validators, so a single test still needs the right
validators running first.

`.agents/rules/testing-and-validation.md` is the source of truth for the suite →
setup-target → test-crate table and the exact commands. **Read its "Isolating
one integration test" and "Suite names and setup targets" sections** rather than
duplicating them here. This skill just drives that workflow.

Assume all required tooling is installed (`cargo`, `cargo nextest`, `make`); do
not check.

## Workflow

1. **Identify the suite** from the target test path, using the table in
   `.agents/rules/testing-and-validation.md`. If unsure, confirm against
   `test-integration/test-runner/bin/run_tests.rs` (suite name + whether it needs
   devnet, ephem, or both).
2. **Build programs** if the `.so` files may be missing/stale:
   `cd test-integration && make programs`.
3. **Terminal A — start only the needed validators** and leave running:
   `make setup-<suite>-{devnet,ephem,both}` (sets `SETUP_ONLY`, waits for Ctrl-C;
   `make list` shows targets).
4. **Terminal B — run the single test** from the owning crate (prefer nextest;
   use `cargo test` when you need `--test-threads=1`/`--exact`). See the examples
   below and the doc for the exact forms.
5. **Stop validators**: Ctrl-C in terminal A. Do not leave them running.

## Examples

`task-scheduler` (devnet only):

```bash
# Terminal A
cd test-integration && make setup-task-scheduler-devnet
# Terminal B
cd test-integration
cargo test -p test-task-scheduler --test test_schedule_magic_cpi_crank test_crank_can_execute_program_that_cpis_into_magic -- --test-threads=1 --nocapture
```

Schedule intents (devnet + ephem):

```bash
# Terminal A
cd test-integration && make setup-schedule-intents-both
# Terminal B
cd test-integration
cargo test -p test-schedule-intent --test 01_invocations test_schedule_commit_directly_with_single_ix -- --test-threads=1 --nocapture
```

## Troubleshooting

- Missing chain account/PDA after setup → inspect the suite config; a missing
  program in the devnet config can fail setup transactions before the asserted
  step. For `task-scheduler`, ensure every program the test touches is in
  `test-integration/configs/schedule-task.devnet.toml`.
- Builds but setup fails → rerun `make programs` in `test-integration`.

## Reporting

Report the suite + setup target used, the exact test command and its pass/fail
result, that validators were stopped, and whether any `.agents/` doc needed
updates.
