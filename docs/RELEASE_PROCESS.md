# Release process

Releases come from `master`, even when the default branch differs. Merge the
intended changes into `master` before preparing a release.

## Prepare

Run the [Prepare release workflow](../.github/workflows/prepare-release.yml)
from the default branch; it checks out `master` itself. Supply `X.Y.Z` or
`vX.Y.Z`, or leave the version empty for the next patch. The workflow rejects
versions that are not newer, existing release branches, and existing tags.

It opens a `release/vX.Y.Z` PR targeting `master`, updates package versions and
Cargo lockfiles, and builds and checks locked dependency resolution for both
Cargo workspaces. These builds do not replace the PR's test gates.

Review the complete diff: version alignment also refreshes selected auxiliary
npm dependencies.

## Validate and publish

1. Run the [package publishing workflow](../.github/workflows/publish-packages.yml)
   manually against `release/vX.Y.Z`, with `release_version` set to `vX.Y.Z`.
   This is a dry run. Wait for it and applicable PR checks to pass.
2. Merge the release PR into `master`.
3. Tag the intended release commit on `master` as `vX.Y.Z` and publish its GitHub
   Release with change and migration notes. This triggers actual publication.
4. Verify successful jobs and expected versions in each destination: GitHub
   validator binaries with embedded TUI support; platform and wrapper npm
   packages; and the `magicblock-magic-program-api` crate. A GitHub
   Release alone does not prove that every output published.

Pushes to `release/v*` only run version-alignment validation; they do not run the
package dry run or publish artifacts.

## Manual version alignment

Run the [alignment script](../.github/version-align.sh) from `.github`:

```sh
(cd .github && ./version-align.sh)
```

It requires `jq` and npm. It reads the root workspace version, updates the npm
manifest and template, and queries npm for the latest VRF oracle, RPC router,
and query-filtering service packages. Failed queries retain existing versions.
It does not generate crates or refresh Cargo lockfiles.

**`--check` also writes files and queries npm** before checking manifest
differences. A newly published auxiliary dependency can therefore cause the
check to fail after preparation.
