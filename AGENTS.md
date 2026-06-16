# Agent Guide

This repository contains AI-agent guidance in `./.agents/`. These files describe the validator's intended behavior, goals, protocol-level expectations, architecture, crate ownership, validation workflow, and documentation-memory rules.

## Required acknowledgement

At the start of any task that may change code, behavior, tests, documentation, configuration, or architecture, the agent **must first read the index at `./.agents/README.md` and explicitly say so** before proceeding. Do not pre-read every document — the index is a routing map that tells you which document to open for a given concern.

Use wording like:

> I read the agent index in `./.agents/README.md` and will open the relevant detailed docs as needed.

If the task is only a trivial file operation and no `./.agents/` file is relevant, say that explicitly.

## Announce available `mbv-*` skills

At startup, before doing task work, the agent **must make the user aware of every `mbv-*` skill** — these are the skills customized for the magicblock-validator. List each by name with its one-line description; do **not** read the skill bodies. Discover them from the skill set available in the environment and/or by listing `.agents/skills/mbv-*/SKILL.md`.

Currently available:

- `mbv-check` — formats, lints, and tests the magicblock-validator Rust workspace (nightly rustfmt, workspace clippy, nextest) with optional error fixing.

Only load a skill (read its `SKILL.md`) when the task actually calls for it.

## Directory layout

- `.agents/rules/` — invariant behavioral and decision-making rules agents must follow.
- `.agents/context/` — static reference context, including overview, architecture, crate map, and crate-specific guides.
- `.agents/memory/` — durable project-memory and documentation-stewardship rules.
- `.agents/specs/` — active protocol/specification notes.
- `.agents/skills/` — executable scripts or capabilities agents can run, when present.
- `.agents/personas/` — specialized agent profiles when this repository needs them.

## Start here

Read `.agents/README.md` first. It is a compact index whose routing table maps each concern (goals, protocol, architecture, crate ownership, validation, memory, per-crate guides) to the single document that covers it. **Open a detailed document only when your task touches that concern** — this keeps the context window small.

Common routes (see the index for the full table):

- behavior/protocol change → `.agents/specs/validator-specification.md`
- is this change aligned? → `.agents/rules/validator-goals.md`
- service wiring/interactions → `.agents/context/architecture.md`
- which crate owns this? → `.agents/context/crate-map.md`, then `.agents/context/crates/<crate>.md`
- how to validate → `.agents/rules/testing-and-validation.md`
- captured durable knowledge → `.agents/memory/agent-memory-and-docs.md`

Before changing code, consult the matching `./.agents` material so the change does not violate the validator's goals, invariants, performance requirements, or specification. This acknowledgement is required; do not proceed silently.

The validator is performance-sensitive infrastructure. Changes must not degrade critical-path performance unless there is no viable alternative; if a tradeoff is unavoidable, call it out explicitly with the reason, expected impact, and any mitigation.

When a feature is added, removed, or changed, the relevant file in `./.agents/` **MUST be updated** to match the current implementation. These files cannot go out of sync with reality; if they do, they lose their usefulness for future agents and maintainers.

When an agent discovers durable repository knowledge that is missing, incomplete, inaccurate, or stale in `./.agents/`—including feature behavior, protocol details, crate responsibilities, validation/debugging workflows, pitfalls, or performance constraints—the agent **MUST** update the most relevant existing document or create a focused new document if none exists. If documentation cannot be updated, the agent must report the blocked follow-up explicitly.

**This obligation is not limited to code-changing tasks.** It applies equally to read-only and question-answering tasks: if you investigate the code to answer a question and learn a durable fact the docs lack or get wrong—especially a divergence from agave/Solana upstream behavior (a missing limit, different default, relaxed validation)—capture it before finishing. In every task's final reply, state whether agent docs were updated, or why no update was needed.

If anything is added to, removed from, renamed, or reorganized inside `./.agents/`, update this `AGENTS.md` file in the same change so this entrypoint remains accurate.
