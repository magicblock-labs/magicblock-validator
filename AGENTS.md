# Agent Guide

Repository guidance lives in `./.agents/`; use `.agents/README.md` as its
routing index.

## Workflow

- Before any repository change or review, read the index, its task-relevant
  documents, and `.agents/rules/invariants.md`. Do not announce document reads
  or available skills; load a skill only when needed.
- Check the diff against all applicable invariants. Violations block creating,
  approving, or recommending merge. End with `Invariants: clear`, or concisely
  report violations and evidence.
- Consult the routed goals, specification, architecture, crate, validation, and
  documentation guidance before changing their respective concerns.
- Preserve critical-path performance. If degradation is unavoidable, report
  its reason, expected impact, and mitigation.

## Pull requests and validation

- Keep documentation out of feature, fix, refactor, and other code pull
  requests; queue it for the manually started weekly documentation task. See
  `.agents/memory/agent-memory-and-docs.md`.
- Before pushing, follow `.github/PULL_REQUEST_TEMPLATE.md` and existing
  conventions: use `type(scope): summary`, include exactly one
  `Closes #<issue>`, and retain `What changed`, `Compatibility`, and
  `Validation`.
- Before pushing a pull-request fix, run only one relevant unit or integration
  test; broader validation belongs in CI. See
  `.agents/rules/testing-and-validation.md`.
- Never put agent, assistant, model, or automation-tool names in GitHub-visible
  metadata, including branches, commits, pull requests, and review replies.

## Repository specifics

- Process roles live in `bins/mbv-leader`, `bins/mbv-verifier`, `bins/mbv`, and
  `bins/mbv-tui`; their shared Keeper image lives in `magicblock-runtime`. See
  `.agents/context/crate-map.md`.
- Validator execution tests use the sibling engine's `testkit` API and v42 test
  program; do not add local test-program crates for that role.
- Follow `.agents/memory/agent-memory-and-docs.md`. Mention agent documentation
  only when updated or when a concrete weekly follow-up is needed.
- When adding, removing, renaming, or reorganizing anything in `./.agents/`,
  update this file in the same documentation-only change.
