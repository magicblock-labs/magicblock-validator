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
- `mbv-run-single-integration-test` — runs a single integration test with the correct validator setup (brings up only the devnet and/or ephem validators the suite needs, then runs one targeted test).

Only load a skill (read its `SKILL.md`) when the task actually calls for it.

## Required invariant review

Before making or reviewing any repository change, including a new pull request
or follow-up changes to an existing pull request, read
`.agents/rules/invariants.md`. Check the proposed or current diff against every
applicable invariant. An invariant violation is blocking: do not create,
approve, or recommend merging the pull request until the violation is resolved.

In the final reply, explicitly state whether the invariant review found any
violation and identify the validation or evidence used for that conclusion.

## Directory layout

- `.agents/rules/` — invariant behavioral and decision-making rules agents must follow.
- `.agents/context/` — static reference context, including overview, architecture, crate map, and crate-specific guides.
- `.agents/memory/` — durable project-memory and documentation-stewardship rules.
- `.agents/specs/` — active protocol/specification notes.
- `.agents/skills/` — executable scripts or capabilities agents can run, when present.
- `.agents/personas/` — specialized agent profiles when this repository needs them.

## Start here

Read `.agents/README.md` first. It is a compact index whose routing table maps each concern (goals, protocol, architecture, crate ownership, validation, memory, per-crate guides) to the single document that covers it. **Open a detailed document only when your task touches that concern** — this keeps the context window small.

For document routing, read ./.agents/README.md; it is the single routing map for this knowledge base.

Before changing code, consult the matching `./.agents` material so the change does not violate the validator's goals, invariants, performance requirements, or specification. This acknowledgement is required; do not proceed silently.

The validator is performance-sensitive infrastructure. Changes must not degrade critical-path performance unless there is no viable alternative; if a tradeoff is unavoidable, call it out explicitly with the reason, expected impact, and any mitigation.

For durable-knowledge and documentation-stewardship requirements, follow `.agents/memory/agent-memory-and-docs.md`. In every task's final reply, state whether agent docs were updated, or why no update was needed.

If anything is added to, removed from, renamed, or reorganized inside `./.agents/`, update this `AGENTS.md` file in the same change so this entrypoint remains accurate.
