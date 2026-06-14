# Agent Guide

This repository contains AI-agent guidance in `./agents/`. These files describe the validator's intended behavior, goals, protocol-level expectations, architecture, crate ownership, and validation workflow.

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
7. Relevant crate-specific guide under `agents/crates/` when one exists, such as `agents/crates/magicblock-chainlink.md` for the `magicblock-chainlink` crate.

Before changing code, consult the relevant `./agents` material to ensure the change does not violate the validator's goals, invariants, or specification. This acknowledgement is required; do not proceed silently after reading the files.

When a feature is added, removed, or changed, the relevant file in `./agents/` **MUST be updated** to match the current implementation. These files cannot go out of sync with reality; if they do, they lose their usefulness for future agents and maintainers.

If anything is added to, removed from, renamed, or reorganized inside `./agents/`, update this `AGENTS.md` file in the same change so this entrypoint remains accurate.
