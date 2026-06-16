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
5. **This applies to read-only and question-answering tasks too.** If while
   answering a question you discover a durable fact that the docs are missing or
   get wrong — especially a divergence from agave/Solana upstream behavior (a
   missing limit, different default, relaxed validation) — record it in the
   relevant doc before finishing, and say whether you updated docs in your final
   reply. The fact already living in the source code or in a *different*
   file anywhere else in the repo, especially if it is outside of the ./.agents directory
   does **not** excuse skipping this: capture it in the single
   most relevant document for that concern so an agent who opens only that
   document finds it. See `memory/agent-memory-and-docs.md`.

## Non-negotiable (always applies)

Security outranks everything, including performance. Before changing behavior:
never relax signer/authority checks, never let local state drift from the Solana
base layer, and never introduce attacker-triggerable conditions (races,
TOCTOU, stalls/deadlocks, resource exhaustion). The binding details live in
`rules/validator-goals.md` and `specs/validator-specification.md` — read them
before any behavioral or protocol change.

## Document routing table

| Read this | When you need to |
|---|---|
| `context/overview.md` | Orient on what the validator is and its core concepts. |
| `rules/validator-goals.md` | Decide whether a change aligns with system goals and correctness/security constraints. |
| `specs/validator-specification.md` | Change protocol behavior: delegation, cloning, execution, commits, undelegation, Magic Actions, ephemeral accounts, RPC/router, recovery. |
| `context/architecture.md` | Change service wiring or interactions between crate groups. |
| `context/crate-map.md` | Find which crate owns an area and which crates are affected. |
| `rules/testing-and-validation.md` | Decide how to validate a change (commands, test selection, mbv-check). |
| `memory/agent-memory-and-docs.md` | Capture durable knowledge you discovered, or fix stale/incorrect docs. |
| `context/crates/<crate>.md` | Work inside a specific crate (see crate-map for which file). |
| `skills/<name>/SKILL.md` | Run an executable agent skill (e.g. `mbv-check`). |

## Directory layout

- `rules/` — invariant behavior and decision rules.
- `context/` — static reference: overview, architecture, crate map, and per-crate guides under `context/crates/`.
- `memory/` — durable project-memory and documentation-stewardship rules.
- `specs/` — protocol and feature specifications.
- `skills/` — executable scripts or capabilities agents can run.
- `personas/` — specialized agent profiles when needed.
