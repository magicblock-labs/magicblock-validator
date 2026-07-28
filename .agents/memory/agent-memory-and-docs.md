# Agent Memory and Documentation Stewardship

This file defines how agents keep repository knowledge current. Treat the files in `./.agents/` as the repository's persistent agent memory. Documentation updates are batched into a dedicated weekly maintenance task, which is currently started manually.

`AGENTS.md`, `.agents/README.md`, overview docs, and crate guides should point here instead of restating this policy.

## Core rule

Do not update documentation in a feature, fix, refactor, or other code pull request. When current guidance is missing, incomplete, inaccurate, or stale, identify the exact documentation follow-up in the task handoff so it can be handled by the weekly documentation-maintenance task.

This applies even when the discovery is incidental to another task. Dedicated documentation tasks, including the weekly maintenance task and explicit instruction-policy changes, may update documentation in documentation-only pull requests.

**Documented elsewhere is not an excuse to omit the follow-up.** A durable fact being present in the source code, a code comment, an unrelated `.agents/` file, an external repo, or any other location does *not* make the relevant agent guidance complete. The test is "would an agent who opens the single most relevant `.agents/` document for this concern find it there?" If the answer is no, name that document and the missing information in the handoff. During weekly maintenance, put the full explanation in the most specific canonical file and add a short pointer from other relevant files.

Concretely: if you investigate code and find that the mechanism, behavior, or invariant you relied on is not spelled out in the crate, specification, or rules document an agent would consult, queue that exact update for the weekly documentation task rather than adding it to the code pull request.

**This rule applies to read-only and question-answering tasks too, not only code changes.** If you learn a durable fact — especially a divergence from agave/Solana upstream behavior, such as a missing limit, different default, or relaxed validation — include the documentation follow-up in the handoff.

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

Apply these steps during the dedicated weekly documentation-maintenance task or another explicitly requested documentation-only task:

Keep updates concise and operational:

1. State the behavior or workflow future agents need to know.
2. Include the owning crate/path/API when relevant.
3. Include validation commands or tests when the discovery changes how work should be checked.
4. Call out performance-sensitive paths and tradeoffs if relevant.
5. Avoid duplicating large blocks across files; link or point to the canonical file instead.

The weekly task should review the durable documentation follow-ups and merged code changes since the previous pass, update the relevant documentation, and submit only documentation changes. Verify that file paths and cross-references remain accurate.

## Final response requirement

When finishing a task, report whether agent documentation was updated. For code tasks, state either that no durable documentation follow-up was needed or list the exact follow-up for the weekly maintenance task.
