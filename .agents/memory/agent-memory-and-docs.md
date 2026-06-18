# Agent Memory and Documentation Stewardship

This file defines how agents keep repository knowledge current. Treat the files in `./.agents/` as the repository's persistent agent memory: when an agent discovers durable information that future agents should rely on, the agent must update these documents in the same change whenever practical.

`AGENTS.md`, `.agents/README.md`, overview docs, and crate guides should point here instead of restating this policy.

## Core rule

Whenever you discover that the current `./.agents/` guidance is missing, incomplete, inaccurate, or stale, update it before finishing the task.

This applies even when the discovery is incidental to another task. Do not leave known gaps for a future agent unless you are blocked from editing documentation; if blocked, report the exact missing update and where it should go.

**Documented elsewhere is not an excuse to skip the update.** A durable fact being present in the source code, a code comment, an unrelated `.agents/` file, an external repo, or any other location does *not* satisfy this rule. The test is not "does this fact exist somewhere?" — it is "would an agent who opens the single most relevant `.agents/` document for this concern find it there?" If the answer is no, you must capture it in that document, even if a related or partial mention already lives in a different file. Each `.agents/` document must be self-sufficient for an agent working in the area it covers; never rely on the reader having read another file. When the same fact is genuinely relevant in two places, put the full explanation in the most specific canonical file and add a short pointer (not a silent omission) from the other.

Concretely: if you investigate code to answer a question and find that the mechanism, behavior, or invariant you relied on is *not* spelled out in the crate/spec/rules document an agent would consult for that area, document it there now — regardless of whether a higher-level or differently-scoped file happens to mention it.

**This rule applies to read-only and question-answering tasks too, not only code changes.** If you investigate the code to answer a question and learn a durable fact — especially a divergence from agave/Solana upstream behavior (e.g. a missing limit, different default, or relaxed validation) — capture it before finishing, then report it per the Final response requirement below.

## What must be captured

Update or create agent documentation when you discover durable information such as:

- a feature, behavior, invariant, lifecycle rule, or protocol detail that is not documented yet;
- a documented behavior that is wrong, misleading, renamed, removed, or implemented differently;
- a new testing, validation, debugging, benchmarking, or operational workflow;
- a crate responsibility, API boundary, dependency, startup/shutdown interaction, or hot-path performance consideration;
- a recurring pitfall, failure mode, race condition, recovery requirement, or security/correctness constraint;
- a new crate-specific area that needs its own guide under `.agents/context/crates/`;
- any other knowledge that future agents should remember to make safe, correct, and efficient changes.

Do not document one-off observations that are only relevant to the current local environment unless they reveal a reusable workflow, constraint, or repository behavior.

## Where to put updates

Prefer updating the most specific existing file:

- `.agents/rules/validator-goals.md` for goals, correctness constraints, and decision criteria.
- `.agents/specs/validator-specification.md` for protocol-level behavior and lifecycle rules.
- `.agents/context/architecture.md` for cross-crate service interactions and boundaries.
- `.agents/context/crate-map.md` for crate ownership, dependencies, consumers, and where to start.
- `.agents/rules/testing-and-validation.md` for validation commands, debugging workflows, and test selection.
- `.agents/context/crates/<crate>.md` for crate-specific behavior, APIs, invariants, pitfalls, or tests.

If no suitable document exists, create a new focused file in `.agents/` or `.agents/context/crates/`. When adding, removing, renaming, or reorganizing agent documentation, update `AGENTS.md` so the entrypoint remains accurate.

## How to update

Keep updates concise and operational:

1. State the behavior or workflow future agents need to know.
2. Include the owning crate/path/API when relevant.
3. Include validation commands or tests when the discovery changes how work should be checked.
4. Call out performance-sensitive paths and tradeoffs if relevant.
5. Avoid duplicating large blocks across files; link or point to the canonical file instead.

When behavior changes in code, update the docs in the same change as the implementation. When the task is documentation-only, verify that file paths and cross-references remain accurate.

## Final response requirement

When finishing a task, report whether agent documentation was updated. If it was not updated, state why no durable agent-memory update was needed, or list the blocked documentation follow-up explicitly.
