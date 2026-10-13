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

Leader integration fixtures require a Yellowstone lifecycle feed on
`127.0.0.1:7801`. Set `YELLOWSTONE_GRPC_CONFIG` to a Geyser JSON config for a
plugin compatible with your local Agave version. CI installs and configures the
plugin through [setup-yellowstone](../.github/actions/setup-yellowstone/action.yml).

Report vulnerabilities privately through the [security policy](SECURITY.md).
For publishing, see the [release process](RELEASE_PROCESS.md).
