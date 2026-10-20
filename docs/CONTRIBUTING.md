# Contributing

Open a PR using the [template](../.github/PULL_REQUEST_TEMPLATE.md).
Use its title convention for commit subjects too.

[MagicBlock Engine](https://github.com/magicblock-labs/magicblock-engine) is the
execution, account storage, and replication layer. Changes to those components
belong in that repository; this workspace owns validator orchestration and
application services.

## Checks

See [validation guidance](../AGENTS.md#validation) for scoped checks.
`make ci-fmt`, `make ci-lint`, and `make ci-test-unit` check this Cargo workspace.

## Validator qualification

Use the RedSuite revision pinned in [validator CI](../.github/workflows/ci-redsuite.yml).
Follow [RedSuite's setup](https://github.com/magicblock-labs/redsuite#-get-started)
at that revision, using this checkout's validator, verifier, and committor program.
From the RedSuite checkout, run:

```sh
redsuite run redshift
redsuite run redhat
redsuite run redline --profile lite
```

Redshift checks correctness. Redhat exercises adversarial ownership, authorization,
and fee cases. Redline measures performance; use `--profile full` for its larger
workloads. A failed run exits nonzero.

Report vulnerabilities privately through the [security policy](SECURITY.md).
For publishing, see the [release process](RELEASE_PROCESS.md).
