# Contributing

Open a PR using the [template](../.github/PULL_REQUEST_TEMPLATE.md).
Use its title convention for commit subjects too.

This workspace owns validator orchestration, execution, account storage,
replication, and application services. Keep changes in the owning crate; see
[repository boundaries](../AGENTS.md#boundaries).

## Checks

See [validation guidance](../AGENTS.md#validation) for scoped checks.
[Integration tests](../test-integration/) use a separate Cargo workspace and
runner: `make ci-test-integration`. `make ci-fmt` and `make ci-lint` cover the root
workspace. Check the integration workspace separately when it is affected:

```sh
cargo +nightly fmt --manifest-path test-integration/Cargo.toml --check -- --config-path rustfmt-nightly.toml
cargo clippy --manifest-path test-integration/Cargo.toml --all-targets -- -D warnings
```

Report vulnerabilities privately through the [security policy](SECURITY.md).
For publishing, see the [release process](RELEASE_PROCESS.md).
