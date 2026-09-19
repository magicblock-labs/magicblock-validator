# Contributing to magicblock-validator

Thanks for contributing. Work on validator processes and application services
belongs here; changes to execution, account storage, or replication belong in
Engine. Prioritize correctness, determinism, and operational safety over speed
of delivery.

Please keep contributions **small, explicit, and reviewable**.

## Scope & Philosophy

- Prefer **minimal changes** over broad refactors.
- Avoid speculative abstractions.
- Treat config, CLI flags, and on-disk formats as **public interfaces**.
- Backward compatibility is the default.

If you are unsure whether a change belongs here, start a **Discussion**.

## Pull Requests

### Title and commit format

PR titles and every non-merge commit subject must follow:

```text
type(scope)!: short summary
```

Where:

- `type` is `feat`, `fix`, `docs`, `style`, `chore`, `refactor`, `test`, `perf`,
  `ci`, `build`, or `revert`
- `scope` and the breaking-change marker `!` are optional
- start the description with lowercase; no trailing whitespace or terminal `.`, `!`, or `?`

Examples:

- `fix: prevent panic on empty slot`
- `feat(rpc): add account snapshot endpoint`

The PR title becomes the commit title when merged.

Under **What changed**, explain why the change is needed and what it does.
Every non-release PR description must include exactly one `Closes #<issue>`
link to the concrete work it completes.

### Compatibility & Safety

Use **Impact** to explain configuration changes, migrations, or behavior changes
that affect operators. **Impact** and **Reviewer notes** are optional in the
[PR template](../.github/PULL_REQUEST_TEMPLATE.md); omit them when there is nothing
useful to add.

### Validation

- Changes affecting correctness, consensus, or state handling **must** be tested.
- Small refactors may rely on existing coverage; explain why if no new tests are added.
- Performance-sensitive changes should include rationale or benchmarks where relevant.
- When validation needs explanation, use **Reviewer notes** for the checks run,
  their results, and any remaining gaps.

Follow [repository guidance](../AGENTS.md) for scoped checks. Documentation-only
changes need link/path checks and `git diff --check`, not a Rust build. For the
full gate, run from the workspace root:

```bash
make ci-fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

The [integration tests](../test-integration/) are a separate supported Cargo
workspace: the root `cargo ... --workspace` commands do not cover it. Use
`make ci-test-integration` for its test runner and `make ci-lint` to lint both
workspaces. `make ci-fmt` already checks both.

## Commits

- Keep commits focused.
- Avoid drive-by formatting or unrelated cleanups.
- Intermediate non-merge commits must also pass the
  [conventions check](../.github/workflows/ci-conventions.yml), even when the PR
  will be squash-merged.

## Code Style

- Follow existing patterns.
- Prefer explicitness over cleverness.
- Avoid macros or unsafe code unless there is a clear, documented need.
- Performance optimizations should be obvious and justified.

## Configs & Interfaces

Operators rely on configuration files, CLI flags, RPC APIs, and on-disk formats.
When changing them, update the documentation and explain compatibility in the
PR. Breaking changes also need a clear migration path.

## Security

If you believe you’ve found a security issue, **do not open a public issue**.  
Use the repository’s [security policy](SECURITY.md) instead.

## Questions & Ideas

- Use **Issues** for concrete, actionable work.
- Use **Discussions** for design questions, ideas, or uncertain proposals.

For version preparation and publishing, follow the [release process](RELEASE_PROCESS.md).

Thanks for helping keep the codebase reliable and straightforward to maintain.
