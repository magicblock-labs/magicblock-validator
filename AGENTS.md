# Agent Guide

This repository contains AI-agent guidance in `./agents/`. These files describe the validator's intended behavior, goals, protocol-level expectations, architecture, crate ownership, validation workflow, and documentation-memory rules.

## Required acknowledgement

At the start of any task that may change code, behavior, tests, documentation, configuration, or architecture, the agent **must first read the relevant files in `./agents/` and explicitly say so** before proceeding.

Use wording like:

> I found and read the relevant agent guidance in `./agents/` and will use it as the source of truth for this change.

If the task is only a trivial file operation and no `./agents/` file is relevant, say that explicitly.

## Start here

Before working on any feature, bug fix, refactor, or behavioral change, read the relevant files:

1. `agents/00_overview.md` — high-level validator purpose and runtime model.
2. `agents/01_validator-goals.md` — system goals and correctness constraints.
3. `agents/02_specification.md` — delegation, cloning, execution, commit, undelegation, Magic Actions, ephemeral accounts, RPC/router, and recovery behavior.
4. `agents/03_architecture.md` — high-level repository architecture and crate interaction model.
5. `agents/04_crate-map.md` — workspace crate purposes, dependencies, consumers, and where to start for common change areas.
6. `agents/05_testing-and-validation.md` — required validation workflow, rs-check guidance, and integration test commands.
7. `agents/06_agent-memory-and-docs.md` — required rules for capturing newly discovered durable behavior, workflows, pitfalls, and documentation corrections.
8. Relevant crate-specific guide under `agents/crates/` when one exists, such as `agents/crates/magicblock-account-cloner.md` for `magicblock-account-cloner`, `agents/crates/magicblock-accounts.md` for `magicblock-accounts`, `agents/crates/magicblock-aml.md` for `magicblock-aml`, `agents/crates/magicblock-aperture.md` for `magicblock-aperture`, `agents/crates/magicblock-api.md` for `magicblock-api`, `agents/crates/magicblock-chainlink.md` for `magicblock-chainlink`, `agents/crates/magicblock-config.md` for `magicblock-config`, `agents/crates/magicblock-core.md` for `magicblock-core`, `agents/crates/magicblock-metrics.md` for `magicblock-metrics`, `agents/crates/magicblock-rpc-client.md` for `magicblock-rpc-client`, `agents/crates/magicblock-services.md` for `magicblock-services`, `agents/crates/magicblock-table-mania.md` for `magicblock-table-mania`, `agents/crates/magicblock-task-scheduler.md` for `magicblock-task-scheduler`, `agents/crates/magicblock-validator.md` for `magicblock-validator`, `agents/crates/magicblock-validator-admin.md` for `magicblock-validator-admin`, `agents/crates/magicblock-version.md` for `magicblock-version`, `agents/crates/storage-proto.md` for `solana-storage-proto`, or `agents/crates/test-kit.md` for `test-kit`.

Before changing code, consult the relevant `./agents` material to ensure the change does not violate the validator's goals, invariants, performance requirements, or specification. This acknowledgement is required; do not proceed silently after reading the files.

The validator is performance-sensitive infrastructure. Changes must not degrade critical-path performance unless there is no viable alternative; if a tradeoff is unavoidable, call it out explicitly with the reason, expected impact, and any mitigation.

When a feature is added, removed, or changed, the relevant file in `./agents/` **MUST be updated** to match the current implementation. These files cannot go out of sync with reality; if they do, they lose their usefulness for future agents and maintainers.

When an agent discovers durable repository knowledge that is missing, incomplete, inaccurate, or stale in `./agents/`—including feature behavior, protocol details, crate responsibilities, validation/debugging workflows, pitfalls, or performance constraints—the agent **MUST** update the most relevant existing document or create a focused new document if none exists. If documentation cannot be updated, the agent must report the blocked follow-up explicitly.

If anything is added to, removed from, renamed, or reorganized inside `./agents/`, update this `AGENTS.md` file in the same change so this entrypoint remains accurate.
