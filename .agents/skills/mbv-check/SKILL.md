---
name: mbv-check
description: Runs the magicblock-validator Rust workspace's full format, lint, and test gate at explicit requests or significant milestones when the whole workspace is expected to pass.
---

# MagicBlock Validator Check Skill

Automated formatting, linting, and testing tailored to the `magicblock-validator`
workspace. This repository is **Rust only**; there are no JS/TS fallbacks.

## Overview

For validation selection policy, see `.agents/rules/testing-and-validation.md`; this skill executes the standard Rust quality gate.

Runs three sequential checks:
1. **Format** — nightly rustfmt with the repo's strict config
2. **Lint** — workspace clippy with warnings denied
3. **Test** — `cargo nextest` across the workspace

## When to run

Loading this skill does not run any command. Run the full gate only when:

- the user explicitly asks for the workspace checks; or
- a significant implementation milestone is complete and the entire workspace
  is expected to format, lint, compile, and test successfully.

Do not run the gate during exploration, after each edit, or while a cross-crate
integration is intentionally incomplete. At intermediate points, use only the
narrow diagnostic command needed to answer the current question or validate the
specific completed boundary. A targeted diagnostic is not a substitute for the
full gate at the final working milestone.

Assume all required tooling is installed and available, including `cargo`, the
nightly toolchain, `cargo fmt`, `cargo clippy`, and `cargo nextest`. Do not check
whether tools are installed and do not hedge about availability.

All commands run from the workspace root.

### Options (optional, user may specify)

- **fix-lint**: Fix clippy findings automatically (default mode)
- **fix-tests**: Attempt to fix test failures automatically
- **no-fix**: Don't fix anything, only report

If no option is specified, use `fix-lint` when the milestone gate is run.

## Workflow

### 1. Format

This repo formats with **nightly** rustfmt and a stricter config
(`rustfmt-nightly.toml`: `imports_granularity = "Crate"`,
`group_imports = "StdExternalCrate"`). Use the repo target:

```bash
make fmt
# equivalent to:
cargo +nightly fmt -- --config-path rustfmt-nightly.toml
```

To only verify without writing (matches CI):

```bash
make ci-fmt
# equivalent to:
cargo +nightly fmt --check -- --config-path rustfmt-nightly.toml
```

Format always runs first regardless of options.

### 2. Lint

```bash
make lint
# equivalent to:
cargo clippy --all-targets -- -D warnings
```

Prefer the full-workspace form for broader changes (matches the documented
baseline in `.agents/rules/testing-and-validation.md`):

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

- With `fix-lint` (default): `cargo clippy --fix --allow-dirty --allow-staged --workspace --all-targets`, then re-run the deny-warnings check above to confirm it is clean.
- With `no-fix`: report findings only.

### 3. Test

Use `cargo nextest` for the workspace test gate:

```bash
cargo nextest run --workspace
```

Targeted forms:

```bash
cargo nextest run -p <crate-name>
cargo nextest run -p <crate-name> <test_name> --no-capture
```

If `nextest` is genuinely unavailable, fall back to `cargo test --workspace`.

- With `fix-tests`: attempt focused fixes for real failures; do not mask root causes.
- With `no-fix`: report failures only.

## Targeted checks

This skill owns the full milestone gate. For an intermediate completed boundary,
choose the smallest relevant package check or test from
`.agents/rules/testing-and-validation.md` without running the rest of this
workflow.

## Notes & guardrails

- Do not run anything merely because the skill was loaded.
- Treat all commands here as ready to run locally; no install/availability checks.
- Format halts nothing; lint and test failures are logged but don't abort the skill.
- This validator is **performance-sensitive and security-critical**; use
  `.agents/rules/testing-and-validation.md` to select any extra focused checks
  needed beyond this standard gate.
- Use `fix-tests` carefully; automatic fixes may not address root causes.

## Reporting

When finishing, report:
- exact commands run and pass/fail for each,
- anything skipped and why,
- any performance-sensitive or security-relevant paths touched and how risk was
  checked or what residual risk remains,
- whether `.agents/` docs needed updates for any durable discovery.
