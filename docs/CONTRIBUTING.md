# Contributing

Open a PR using the [template](../.github/PULL_REQUEST_TEMPLATE.md).
Use its title convention for commit subjects too.

[MagicBlock Engine](https://github.com/magicblock-labs/magicblock-engine) is the
execution, account storage, and replication layer. Changes to those components
belong in that repository; this workspace owns validator orchestration and
application services.

## Checks

See [validation guidance](../AGENTS.md#validation) for scoped checks.
Run CI's formatting, lint, and unit checks directly with Cargo:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
RUST_BACKTRACE=1 cargo nextest run
```

Automatic CI runs for PRs and pushes to `master` or `dev` that change `.rs` or
`.toml` files, including RedSuite's Redshift correctness and Redhat security
scenarios. Manual runs and published releases bypass this filter. Redline
benchmarks run only on published releases.

## Validator qualification

Follow [RedSuite's setup guide](https://github.com/magicblock-labs/redsuite/blob/c72172c0444f5857af17581a9e8dd2073b9bc107/README.md#-get-started)
using the [revision pinned by CI](../.github/actions/run-redsuite/action.yml)
and this checkout's validator, verifier, and committor program.
From the RedSuite checkout, run:

```sh
redsuite redshift run
redsuite redhat run
redsuite redline run --profile lite
```

Redshift checks correctness. Redhat exercises adversarial ownership, authorization,
and fee cases. Redline measures performance; use `--profile full` for its larger
workloads. A failed run exits nonzero.

Report vulnerabilities privately through the [security policy](SECURITY.md).
For publishing, see the [release process](RELEASE_PROCESS.md).
