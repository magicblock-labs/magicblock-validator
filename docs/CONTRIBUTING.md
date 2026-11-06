# Contributing

Use the [issue templates](../.github/ISSUE_TEMPLATE/) for bug reports, questions,
and change proposals. Report vulnerabilities privately through the
[security policy](SECURITY.md).

Open pull requests using the [template](../.github/PULL_REQUEST_TEMPLATE.md).
Use its title convention for commit subjects too. Contributors are expected to
follow the [code of conduct](CODE_OF_CONDUCT.md).

[MagicBlock Engine](https://github.com/magicblock-labs/magicblock-engine) is the
execution, account storage, and replication layer. Changes to those components
belong in that repository; this workspace owns validator orchestration and
application services.

## Getting started

Install Rust through `rustup`; the checkout selects its toolchain through
[rust-toolchain.toml](../rust-toolchain.toml). Native builds require a C/C++
toolchain, libclang, and the Protobuf compiler (`protoc`).

From the workspace root, build the leader and verifier:

```sh
cargo build --locked -p magicblock-validator -p magicblock-verifier
```

See [validator setup](../bins/validator/README.md) to configure and run a leader,
or [verifier setup](../bins/verifier/README.md) to follow an existing leader.

## Checks

Install [cargo-nextest](https://nexte.st/docs/installation/pre-built-binaries/) to
run the tests.
Use `-p <package>` to scope Clippy and tests to the crates affected by your change.
The workspace-wide checks are:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

## Validator qualification

Use [RedSuite](https://github.com/magicblock-labs/redsuite) for validator-backed
correctness, security, and performance checks. Follow its
[setup guide](https://github.com/magicblock-labs/redsuite#-get-started) to build the
fixtures and run scenarios against this checkout's leader, verifier, and
committor program. Scenario selection and benchmark options are documented there.

For publishing, see the [release process](RELEASE_PROCESS.md).
