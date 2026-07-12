# Agent Knowledge Base — Index

This directory is the repository-local knowledge base for AI coding agents.

**This file is the index. Read only this file up front.** Do not pre-read the
other documents. Use the routing table below to open a specific document *only
when the task actually needs that information*. This keeps the agent's context
window small.

## How to use this index

1. Read this index to learn what exists and where it lives.
2. Identify what your task touches (behavior, protocol, wiring, a crate, tests, docs).
3. Open only the matching document(s) from the tables below.
4. If a change adds/removes/renames knowledge, update the relevant doc and, if
   the layout changes, update this index and `../AGENTS.md`.
5. If you discover missing or stale durable repository knowledge, follow
   `memory/agent-memory-and-docs.md` before finishing.

## Non-negotiable (always applies)

Read `rules/invariants.md` before making or reviewing any repository change or
pull request. Verify that the proposed or current diff does not break any
applicable invariant; an invariant violation blocks the change.

Security outranks performance; read `rules/validator-goals.md` and
`specs/validator-specification.md` before behavioral or protocol changes.

## Document routing table

| Read this | When you need to |
|---|---|
| `rules/invariants.md` | Check every repository change or pull request against the validator and runtime correctness invariants. |
| `context/overview.md` | Orient on what the validator is and its core concepts. |
| `rules/validator-goals.md` | Decide whether a change aligns with system goals and correctness/security constraints. |
| `specs/validator-specification.md` | Change protocol behavior: delegation, cloning, execution, commits, undelegation, Magic Actions, ephemeral accounts, RPC/router, recovery. |
| `context/architecture.md` | Change service wiring or interactions between crate groups. |
| `context/crate-map.md` | Find which crate owns an area and which crates are affected. |
| `rules/testing-and-validation.md` | Decide how to validate a change (commands, test selection, mbv-check). |
| `memory/agent-memory-and-docs.md` | Capture missing repository facts you discovered, or fix stale/incorrect docs. |
| `context/crates/<crate>.md` | Work inside a specific crate (see crate-map for which file). |
| `skills/<name>/SKILL.md` | Run an executable agent skill (e.g. `mbv-check`). |

## Directory layout

- `rules/` — invariant behavior and decision rules.
- `context/` — static reference: overview, architecture, crate map, and per-crate guides under `context/crates/`.
- `memory/` — durable project-memory and documentation-stewardship rules.
- `specs/` — protocol and feature specifications.
- `skills/` — executable scripts or capabilities agents can run.
- `personas/` — specialized agent profiles when needed.
