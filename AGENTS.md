# Agent Guide

This repository contains AI-agent guidance in `./agents/`. These files describe the validator's intended behavior, goals, and protocol-level expectations.

Start here before working on any feature, bug fix, refactor, or behavioral change:

1. `agents/00-overview.md` — high-level validator purpose and runtime model.
2. `agents/01-validator-goals.md` — system goals and correctness constraints.
3. `agents/02-specification.md` — delegation, cloning, execution, commit, undelegation, Magic Actions, ephemeral accounts, RPC/router, and recovery behavior.

Before changing code, consult the relevant `./agents` material to ensure the change does not violate the validator's goals, invariants, or specification.

When a feature is added, removed, or changed, the relevant file in `./agents/` **MUST be updated** to match the current implementation. These files cannot go out of sync with reality; if they do, they lose their usefulness for future agents and maintainers.

If anything is added to, removed from, renamed, or reorganized inside `./agents/`, update this `AGENTS.md` file in the same change so this entrypoint remains accurate.
