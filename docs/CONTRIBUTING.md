# Contributing

Open a PR using the [template](../.github/PULL_REQUEST_TEMPLATE.md).
Use its title convention for commit subjects too.

[MagicBlock Engine](https://github.com/magicblock-labs/magicblock-engine) is the
execution, account storage, and replication layer. Changes to those components
belong in that repository; this workspace owns validator orchestration and
application services.

## Checks

See [validation guidance](../AGENTS.md#validation) for scoped checks.
[Integration tests](../test-integration/) use a separate Cargo workspace and
runner: `make ci-test-integration`. `make ci-fmt` and `make ci-lint` cover both
workspaces.

Report vulnerabilities privately through the [security policy](SECURITY.md).
For publishing, see the [release process](RELEASE_PROCESS.md).
