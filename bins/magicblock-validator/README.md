# magicblock-validator

Runs the leader of an Ephemeral Rollup: application RPC, account synchronization,
settlement, and recurring tasks. Engine provides execution, storage, and
replication to allowed followers.

## Run a leader

Prepare a configuration using the [leader example](../../config.example.toml).
Replace sample identities and provider credentials, choose storage and listener
addresses, and supply the configured program executable files.

From the workspace root:

```bash
cargo run --release --locked -p magicblock-validator -- --config config.toml
```

Command-line options override environment variables, which override the TOML
file. See [configuration](../../magicblock-config/README.md) for details, or
append `--help` for available options.

## Operating it

Startup can perform base-chain setup, including fee-vault initialization.
Domain registration is separate: use the [operator CLI](../magicblock/README.md).

Unexpected managed-service exits stop the process. Use its logs and
[metrics](../../magicblock-metrics/README.md) to diagnose failures.
For a replication follower without application services, run the
[verifier](../magicblock-verifier/README.md).

See [deployment prerequisites](https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/deployment-prerequisites.md)
and [recovery](https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/recovery.md)
for multi-host operation.

[Back to workspace](../../README.md)
