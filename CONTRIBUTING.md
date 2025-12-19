# Contributing to magicblock-validator

Thanks for contributing. This repository implements a high-performance, long-running solana execution engine.  
Correctness, determinism, and operational safety matter more than velocity.

Please keep contributions **small, explicit, and reviewable**.

---

## Scope & Philosophy

- Prefer **minimal changes** over broad refactors.
- Avoid speculative abstractions.
- Treat config, CLI flags, and on-disk formats as **public interfaces**.
- Backward compatibility is the default.

If you are unsure whether a change belongs here, start a **Discussion**.

---

## Pull Requests

### Title format
PR titles must follow:

type(scope): short summary

Where:
- `type` ∈ `feat | fix | docs | chore | refactor | test | perf | ci | build`
- `scope` is optional
- use lowercase, no trailing period

Examples:
- `fix: prevent panic on empty slot`
- `feat(rpc): add account snapshot endpoint`

The PR title becomes the commit title when merged.

---

### Compatibility & Safety

Explicitly call out any of the following in the PR description:
- config changes
- migrations (disk, state, network, protocol)
- behavior changes affecting operators

If none apply, mark the change as **non-breaking**.

---

### Testing

- Changes affecting correctness, consensus, or state handling **must** be tested.
- Small refactors may rely on existing coverage; explain why if no new tests are added.
- Performance-sensitive changes should include rationale or benchmarks where relevant.

---

## Commits

- Keep commits focused.
- Avoid drive-by formatting or unrelated cleanups.
- Squash merges are used; intermediate commit messages are not critical.

---

## Code Style

- Follow existing patterns.
- Prefer explicitness over cleverness.
- Avoid macros or unsafe code unless there is a clear, documented need.
- Performance optimizations should be obvious and justified.

---

## Configs & Interfaces

Assume that:
- config files
- CLI flags
- RPC / API surfaces
- on-disk formats

are relied upon by external operators.

Changes here require:
- documentation updates
- compatibility notes in the PR
- clear migration paths if breaking

---

## Security

If you believe you’ve found a security issue, **do not open a public issue**.  
Use the repository’s security policy instead.

---

## Questions & Ideas

- Use **Issues** for concrete, actionable work.
- Use **Discussions** for design questions, ideas, or uncertain proposals.

---

Thanks for helping keep the codebase fast, predictable, and boring in the best way.
