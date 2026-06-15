# `magicblock-version`

## Purpose

`magicblock-version` is the validator's shared build/version metadata crate. It provides the `Version` value used by the validator binary, the JSON-RPC `getVersion` response, and operator-facing TUI/headless startup displays.

High-level responsibilities:

- derive the MagicBlock package semver from the workspace package version at compile time;
- expose Solana compatibility metadata, including the Agave RPC API version and current `solana-feature-set` identifier;
- expose build source metadata, including a CI-provided commit prefix and a compile-time git description from `git-version`;
- identify the client implementation as MagicBlock while preserving Solana/Jito/Firedancer client ID compatibility;
- keep the version type serializable, sanitizable, and ABI-example-compatible for RPC/operator consumers.

This crate is small and dependency-light. It is not on the transaction execution hot path, but it is operator- and RPC-facing: field names, formatting, and client IDs are compatibility-sensitive because `magicblock-aperture`, `magicblock-validator`, and TUI/operator tooling depend on them.

## Update requirement

Update this guide in the same change whenever `magicblock-version` behavior or contracts change. In particular, update it for changes to:

- `Version` fields, serialization behavior, `Display` / `Debug` formatting, or the `semver!` / `version!` macros;
- how `major`, `minor`, `patch`, `commit`, `feature_set`, `client`, `solana_core`, or `git_version` are computed;
- the `ClientId` numeric mapping or accepted unknown-client behavior;
- build-script cfg handling for stable/beta/nightly/dev Rust toolchains;
- consumers that expose version data through RPC, startup logs, the embedded/external TUI, packaging, or operator docs;
- validation commands or tests used to check version compatibility.

Also update this file if another crate changes a contract that consumes this crate, such as the `getVersion` JSON shape in `magicblock-aperture` or startup/TUI version display in `magicblock-validator`.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-version/Cargo.toml` | Crate manifest. Depends on `semver`, `serde`, Solana feature/RPC/sanitize/ABI crates, and `git-version`; uses `rustc_version` in the build script. |
| `magicblock-version/build.rs` | Emits `rustc-check-cfg` declarations for specialization-related cfgs based on the active compiler channel. |
| `magicblock-version/src/lib.rs` | Defines `ClientId`, public `Version`, default metadata computation, formatting, macros, `Sanitize`, and unit tests. |
| `Cargo.toml` | Workspace package version is the source for `Version.major`, `minor`, and `patch`. |
| `magicblock-aperture/src/requests/http/get_version.rs` | RPC consumer. Builds the `getVersion` response from `Version::default()`. |
| `magicblock-aperture/tests/node.rs` and `magicblock-aperture/tests/batches.rs` | RPC tests that assert `getVersion` returns Solana version/feature-set information and works in batches. |
| `magicblock-validator/src/main.rs` | Binary consumer. Prints headless startup version/git metadata and passes version fields into the embedded TUI config. |
| `tools/magicblock-tui-client/src/app.rs` and `tools/magicblock-tui-client/src/state.rs` | TUI consumer. External TUI starts with its own package version, then enriches display with the validator's `getVersion` `solana-core` value when reachable. |

Main consumers:

- `magicblock-aperture` exposes version metadata through Solana-compatible JSON-RPC.
- `magicblock-validator` uses it for headless startup output and embedded TUI configuration.
- Operator tooling and external Solana clients indirectly consume the JSON returned by `getVersion`.

Important upstream dependencies:

- `solana-feature-set::ID` supplies the feature-set fingerprint exposed to RPC clients.
- `solana_rpc_client_api::response::RpcApiVersion::default()` supplies the Solana core/API version string.
- `git-version::git_version!()` supplies `Version.git_version` at compile time.
- `CI_COMMIT`, when set, is parsed by `compute_commit` as the first four bytes of a hex SHA-1 prefix.

## Public API shape / Main public types and APIs

Public surface from `src/lib.rs`:

- `pub struct Version` with public fields:
  - `major`, `minor`, `patch`: parsed from `CARGO_PKG_VERSION_*`;
  - `commit`: first four bytes of `CI_COMMIT` interpreted as big-endian hex, or `0` when absent/invalid;
  - `feature_set`: first four bytes of `solana_feature_set::ID`, interpreted as little-endian;
  - `solana_core`: default Solana RPC API version string;
  - `git_version`: `git-version` output string.
- `client: u16` is intentionally private but participates in serialization because it is a field of `Version`. `Version::default()` sets it to the MagicBlock client ID.
- `Version::as_semver_version() -> semver::Version` returns only the MagicBlock package semver.
- `impl Default for Version` is the canonical constructor. Consumers should prefer `Version::default()` instead of recomputing build metadata.
- `impl Display` renders `major.minor.patch` only. This is used for `magicblock-core` in `getVersion` and for validator startup display.
- `impl Debug` renders `major.minor.patch (src:<commit>; feat:<feature_set>, client:<ClientId>)`. The `version!()` macro returns this debug string.
- `semver!()` returns a formatted `Display` string for `Version::default()`.
- `version!()` returns a formatted `Debug` string for `Version::default()`.
- `impl Sanitize for Version` is intentionally empty, matching Solana's lightweight sanitize marker pattern for this metadata type.

`ClientId` is crate-private but compatibility-sensitive:

| Numeric ID | Meaning |
|---|---|
| `0` | SolanaLabs |
| `1` | JitoLabs |
| `2` | Firedancer |
| `3` | MagicBlock |
| `4..=u16::MAX` | Preserved as `Unknown(id)` |

`TryFrom<ClientId> for u16` rejects `Unknown(0..=3)` so known IDs cannot be smuggled through the unknown variant.

## Runtime flows

### Constructing default version metadata

```text
Version::default()
  -> parse CARGO_PKG_VERSION_MAJOR/MINOR/PATCH
  -> compute feature_set from solana_feature_set::ID[..4]
  -> compute commit from option_env!("CI_COMMIT") first 8 hex chars, else 0
  -> set client to ClientId::MagicBlock numeric ID 3
  -> read Solana RPC API version from RpcApiVersion::default()
  -> read git_version from git_version::git_version!()
```

Pitfalls:

- `CI_COMMIT` is optional and may be non-hex (for example `HEAD` in local builds). Invalid values must continue to degrade to `0` rather than failing startup or build.
- `feature_set` uses little-endian bytes from the feature-set ID; changing byte order would change the RPC-visible value.
- `Display` intentionally omits git and feature metadata. Consumers that need git metadata must use `Version.git_version` or `Debug`/`version!()`.

### `getVersion` RPC flow

```text
HTTP getVersion request
  -> magicblock-aperture HttpDispatcher::get_version
  -> Version::default()
  -> JSON result fields:
       solana-core   = version.solana_core
       feature-set   = version.feature_set
       git-commit    = version.git_version
       magicblock-core = version.to_string()
```

The RPC field names are external compatibility surface. Do not rename them or swap `git-commit` from `git_version` to the numeric `commit` field without updating RPC tests, docs, and downstream tooling expectations.

### Validator/TUI display flow

```text
magicblock-validator startup
  -> Version::default()
  -> headless output: "Validator version: <Display> (Git: <git_version>)"
  -> embedded TUI config: version=<Display>, git_version=<git_version>

external TUI startup
  -> starts with tools/magicblock-tui-client package version/GIT_HASH
  -> calls getVersion when validator RPC is reachable
  -> appends validator <solana-core> to the displayed version string
```

Preserve the distinction between the validator crate's own version metadata and the external TUI binary's package/GIT_HASH metadata.

## Important internals and caveats

### Build-script cfg declarations

`build.rs` uses `rustc_version::version_meta()` to emit check-cfg declarations for `RUSTC_WITH_SPECIALIZATION` or `RUSTC_WITHOUT_SPECIALIZATION`. `src/lib.rs` has `#![cfg_attr(RUSTC_WITH_SPECIALIZATION, feature(min_specialization))]` to mirror Solana's version crate pattern.

Current behavior only declares the cfg names for stable/beta/nightly and sets `RUSTC_NEEDS_PROC_MACRO_HYGIENE` on dev toolchains. It does not currently emit `cargo:rustc-cfg=RUSTC_WITH_SPECIALIZATION` for nightly or `RUSTC_WITHOUT_SPECIALIZATION` for stable/beta. If this changes, validate on the supported toolchains because cfg spelling and Cargo output syntax directly affect builds.

### Commit metadata sources

There are two commit-like fields:

- `commit: u32` is derived from `CI_COMMIT` and appears in `Debug` as `src:<hex>`.
- `git_version: String` comes from `git-version` and is exposed by RPC as `git-commit` and by startup/TUI display as Git metadata.

Do not assume they are identical. Local builds may have `commit == 0` while `git_version` contains a git description.

### Serialization compatibility

`Version` derives `Serialize`/`Deserialize`, and the private `client` field is still serialized by serde. Adding, removing, renaming, or changing field visibility/defaults can affect any Solana-compatible client or persisted/test payload that deserializes this type.

## Important invariants

1. `Version::default()` must be cheap, deterministic within one binary build, and side-effect free. It should not perform I/O, network calls, or runtime git commands.
2. `Display` must remain the MagicBlock package semver string (`major.minor.patch`) unless every consumer display/RPC expectation is updated.
3. The MagicBlock client ID must remain `3` unless coordinated with Solana-client compatibility expectations and all tests/docs are updated.
4. Unknown client IDs `>= 4` must round-trip through `ClientId::Unknown` and `TryFrom<ClientId> for u16`.
5. Known client IDs must not be accepted through `ClientId::Unknown(0..=3)`.
6. Invalid or absent `CI_COMMIT` must not fail builds or validator startup; it should continue to produce `commit == 0`.
7. `getVersion` response field names and meanings must remain stable for Solana/RPC/operator tooling.
8. Keep this crate dependency-light. Do not add runtime dependencies that pull heavy validator services into version reporting.

## Common change areas and what to inspect

### Changing package/version reporting

Start with:

- `Cargo.toml` `[workspace.package].version`;
- `magicblock-version/src/lib.rs` `Version::default`, `Display`, and `as_semver_version`;
- `magicblock-aperture/src/requests/http/get_version.rs` for RPC field mapping;
- `magicblock-validator/src/main.rs` for startup/TUI display.

Check that `magicblock-core` in RPC continues to mean the MagicBlock validator semver, while `solana-core` continues to mean the Solana RPC API version string.

### Changing client IDs

Start with:

- `ClientId` enum;
- `impl From<u16> for ClientId`;
- `impl TryFrom<ClientId> for u16`;
- `test_client_id`.

Preserve known-ID rejection through `Unknown` and update this guide with any new numeric assignment.

### Changing commit/git metadata

Start with:

- `compute_commit` and `test_compute_commit`;
- `Version::default` `CI_COMMIT` and `git_version::git_version!()` usage;
- `magicblock-aperture/src/requests/http/get_version.rs` `git-commit` mapping;
- `magicblock-validator/src/main.rs` startup display.

Be explicit about whether a change affects the numeric `commit`, string `git_version`, or RPC field named `git-commit`.

### Changing build-script/toolchain behavior

Start with:

- `magicblock-version/build.rs`;
- `magicblock-version/Cargo.toml` `unexpected_cfgs` check-cfg allowlist;
- the crate-level `cfg_attr(RUSTC_WITH_SPECIALIZATION, feature(min_specialization))`.

Validate with the repository-supported toolchain. If you alter emitted cfgs, ensure Cargo output keys use the intended spelling (`cargo:...` versus `cargo::...`) for the minimum supported Cargo version.

## Tests and validation

For documentation-only changes touching this guide, at minimum verify changed paths and cross-references:

```bash
git diff --check
```

For changes to `magicblock-version` itself, run:

```bash
cargo fmt
cargo test -p magicblock-version
```

For RPC-visible changes, also run the relevant Aperture tests:

```bash
cargo test -p magicblock-aperture --test node test_get_version
cargo test -p magicblock-aperture --test batches test_batch_requests
```

Before handing off Rust behavior changes, follow the broader baseline from `agents/05_testing-and-validation.md` when time allows:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance risk is normally low because this crate only builds small metadata values. Still report any new dependency, I/O, process spawning, locking, or allocation-heavy behavior added to `Version::default()` because `getVersion` and startup display call it synchronously.

## Related docs

- `agents/00_overview.md` for the validator's high-level runtime model.
- `agents/03_architecture.md` for process/API ingress boundaries.
- `agents/04_crate-map.md` for workspace crate ownership and consumers.
- `agents/05_testing-and-validation.md` for repository validation workflow.
- `agents/crates/magicblock-aperture.md` for the RPC layer that exposes `getVersion`.
- `agents/crates/magicblock-validator.md` for startup/headless/TUI version display.
