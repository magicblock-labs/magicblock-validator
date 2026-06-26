---
name: mbv-run-single-integration-test
description: Runs a single magicblock-validator integration test with the correct validator setup. Brings up only the devnet and/or ephemeral validators a suite needs (mirroring CI), then runs one targeted test from the owning crate. Use when you need to run or debug one specific integration test or test function locally.
---

# Run Single Integration Test Skill

Run one integration test in the `magicblock-validator` repo with the correct
validator topology; integration tests live under `test-integration/` and need the
right devnet and/or ephemeral validators running first.

`.agents/rules/testing-and-validation.md` is the source of truth for integration
suite mappings, setup targets, and exact command syntax. Read its "Isolating one
integration test" and "Suite names and setup targets" sections before running
this skill.

Assume all required tooling is installed (`cargo`, `cargo nextest`, `make`); do
not check.

## Workflow

1. Identify the owning integration suite and test crate from the target test path,
   using `.agents/rules/testing-and-validation.md`; if uncertain, confirm against
   `test-integration/test-runner/bin/run_tests.rs`.
2. Build SBF programs if they may be missing or stale, following the exact command
   in the testing rules.
3. Start only the required validators for that suite in one terminal and leave
   them running.
4. Run the requested test from the owning integration crate in another terminal,
   using the exact nextest or cargo-test form from the testing rules.
5. Stop the validators with Ctrl-C; do not leave them running.

## Troubleshooting

- Missing chain account/PDA after setup → inspect the suite config; a missing
  program in the devnet config can fail setup transactions before the asserted
  step. For `task-scheduler`, ensure every program the test touches is in
  `test-integration/configs/schedule-task.devnet.toml`.
- Builds but setup fails → rebuild the integration programs using the command in
  `.agents/rules/testing-and-validation.md`.

## Reporting

Report the suite + setup target used, the exact test command and its pass/fail
result, that validators were stopped, and whether any `.agents/` doc needed
updates.
