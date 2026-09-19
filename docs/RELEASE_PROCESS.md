# Release process

Start by merging the intended release changes into `master`. Releases are cut
from that branch, even when the repository's default branch is different;
preparation does not bring in changes from `dev`.

## Prepare the release PR

Run the [Prepare release workflow](../.github/workflows/prepare-release.yml)
from the repository's default branch. The workflow checks out `master` itself.
Supply a version such as `0.15.4` or `v0.15.4`, or leave it empty for the next
patch version. The version must be newer than the workspace version; existing
release branches and tags are rejected.

You will get a `release/vX.Y.Z` branch and a PR targeting `master`, with the
workspace version, npm manifests, and Cargo lockfiles updated. Preparation builds
both the root and `test-integration` workspaces and checks their lockfiles with
full `cargo metadata --locked` resolution.

Review the complete diff: version alignment also refreshes selected auxiliary
npm dependencies, so the changes may go beyond the version bump. The preparation
builds do not replace the PR's test gates.

## Version alignment

For manual preparation, run the [alignment script](../.github/version-align.sh)
from `.github`, not the repository root:

```bash
(cd .github && ./version-align.sh)
```

The script requires `jq` and npm. It reads the root workspace version and updates
the npm package manifest and template. It also queries npm for the latest VRF
oracle, RPC router, and query-filtering service packages; failed lookups leave
their existing versions in place. Rust workspace members inherit the workspace
version; the script does not generate crates or refresh Cargo lockfiles.

**`--check` is not read-only.** It performs the same writes and npm queries before
checking for manifest differences. A newly published auxiliary dependency can
therefore cause an alignment check to fail after preparation.

## Validate and publish

Pushes to `release/v*` trigger a dry run of the
[package publishing workflow](../.github/workflows/publish-packages.yml).
Running it manually is also a dry run. Publishing a GitHub Release is what
enables package publication and release-asset uploads.

1. Wait for the release PR's applicable CI checks and package dry runs to pass.
2. Merge the release PR into `master`.
3. Create the `vX.Y.Z` tag at the intended release commit on `master` and publish
   its GitHub Release with notes describing changes and migration requirements.
4. Verify the publishing workflow completed successfully, not just that the
   GitHub Release exists.

Published outputs include the validator, verifier, operator CLI, and TUI release
binaries; platform npm packages and the wrapper package; and the
`magicblock-magic-program-api` crate. Check the expected versions and artifacts
in each destination. Do not assume all outputs published if a job failed.

[Back to contributing](CONTRIBUTING.md) · [Back to workspace](../README.md)
