---
name: mbv-engine-integration
description: Maintain and resume the magicblock-validator integration with the sibling ../engine repository through one local progress file. Use when planning, implementing, testing, reviewing, or resuming the MBV engine integration and runtime-replacement refactor.
---

# MBV Engine Integration

Maintain `.agents/memory/engine-integration.local.md` as the single source of
session-to-session integration context. Keep it concise enough to read in full
at the start of every integration task.

## Start or resume work

1. Resolve the MBV root with `git rev-parse --show-toplevel` and confirm that
   `../engine` is the sibling engine repository.
2. Follow `AGENTS.md`: announce the available `mbv-*` skills, read
   `.agents/README.md`, then read the routed documents required by the task.
   Read `.agents/rules/invariants.md` before reviewing or changing repository
   state. Read `../engine/AGENTS.md` before work involving engine code.
3. Read the progress file in full. If it does not exist, create it with the
   section layout below.
4. Reconcile its repository state with compact Git facts from both repositories:
   branch, HEAD, status, upstream divergence, and a short diff summary against
   the recorded base. Treat live repository and test results as authoritative.
5. State the current goal, known blocker, and next action before starting work.

Do not emit a full integration diff unless the user asks for it. Prefer
`git diff --shortstat`, targeted `git diff --stat`, and symbol-level inspection.

## Maintain the progress file

Use these headings in this order:

```markdown
# MBV Engine Integration

Last updated: <timestamp and timezone>

## Repositories
## Goal
## Boundaries
## Current state
## Active plan
## Decisions
## Blockers
## Validation
## Completed milestones
## Handoff
```

Update the file after each meaningful milestone and immediately before the
final reply. Record:

- current branches, HEADs, upstream divergence, and whether either tree is dirty;
- the active ordered plan and the single next action;
- durable decisions and cross-repository ownership boundaries;
- blockers with exact diagnostics and owning paths;
- exact validation commands, outcomes, and relevant dates;
- invariant, security, performance, and documentation findings;
- concise completed milestones and enough handoff context to resume directly.

Preserve user-authored decisions. Correct stale facts rather than accumulating
contradictory entries. Summarize results instead of copying raw build logs or
large diffs. Keep all progress and planning context in this file; do not create
parallel status documents.

## Respect repository boundaries

- Keep validator policy, commit decisions, deployment policy, and protocol
  coordination in MBV.
- Keep `../engine` focused on executing caller-loaded transactions and returning
  account changes. Do not introduce validator policy into the engine.
- Read `../engine/solana/README.md` before changing its Solana fork crates.
- Apply MBV's invariant and documentation-memory rules to every MBV change.
- Record a performance assessment for runtime-critical changes, even when no
  benchmark is practical.

The skill tracks work; it does not authorize unrelated changes. Perform only
the task requested by the user and use the narrowest validation that proves it.

## Hand off

Before finishing:

1. Refresh both repositories' branch, HEAD, status, and divergence facts.
2. Update the active plan, blockers, validation results, completed milestones,
   and exact next action.
3. Record whether the invariant review found a violation, whether performance
   was measured, and whether agent documentation changed.
4. Confirm the progress file remains ignored and absent from `git status`.
