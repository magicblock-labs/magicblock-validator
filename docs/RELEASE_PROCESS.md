# Release process

Prepare each release from the branch that maintains its release line. Merge the
intended changes there before preparing a release.

## Prepare

Run the [Prepare release workflow](../.github/workflows/prepare-release.yml)
on the selected release-line branch; it uses that branch's workflow
and source. Supply `X.Y.Z` or `X.Y.Z-rc.N`, optionally prefixed with `v`.
Leaving the version empty promotes the current RC to stable, or selects the
next patch for a stable version. The workflow rejects versions that are not
newer, existing release branches, and existing tags.

It opens a `release/v<version>` PR targeting the selected release line, aligns
package versions, and updates local package versions in the Cargo lockfiles
without re-resolving third-party dependencies. It builds the root workspace and
`test-integration` when present, using `--locked`. These builds do not replace
the PR's test gates.

Review the complete diff: version alignment also refreshes selected auxiliary
npm dependencies.

## Validate and publish

1. Run the [package publishing workflow](../.github/workflows/publish-packages.yml)
   manually against `release/v<version>`, with `release_version` set to
   `v<version>`. For stable versions this builds and dry-runs publication;
   RCs skip artifact/package publication jobs. Wait for applicable checks to pass.
2. Merge the release PR into its release line. The [tag workflow](../.github/workflows/release-tag.yml)
   creates an annotated tag at the commit that introduced the version and opens
   a draft GitHub Release. Do not create release tags manually.
3. Review the draft's tag target and release notes. Publish the stable release
   to trigger actual distribution. Keep testing-only RCs as draft prereleases;
   RC/prerelease builds do not publish binaries, npm packages, or the crate.
4. Verify successful jobs and expected versions in each destination: GitHub
   binaries for the validator, verifier, operator CLI, and TUI, plus the
   platform-independent `v42_calculator_program.so`; platform and wrapper npm
   packages; and the `magicblock-magic-program-api` crate. A GitHub
   Release alone does not prove that every output published.

The publishing workflow builds v42 once from the Engine revision in the validator's
`Cargo.lock`. Deployment hosts download the ELF from the same release as the native
binaries; they do not need an SBF toolchain.

Stable releases on the current line use the npm `latest` channel and the latest
GitHub Release designation. Legacy releases use the npm `legacy` channel and do
not replace the latest GitHub Release. The publishing workflow owns this policy.

Pushes to `release/v*` only run version-alignment validation; they do not run the
package dry run or publish artifacts.

## Manual version alignment

Run the [alignment script](../.github/version-align.sh) from `.github`:

```sh
(cd .github && ./version-align.sh)
```

It requires `jq` and npm. It reads the root workspace version, updates the npm
manifest and template, and queries npm for the latest VRF oracle, RPC router,
query-filtering service, and Hydra cranker packages. Failed queries retain existing versions.
It does not generate crates or refresh Cargo lockfiles.

**`--check` also writes files and queries npm** before checking manifest
differences. A newly published auxiliary dependency can therefore cause the
check to fail after preparation.
