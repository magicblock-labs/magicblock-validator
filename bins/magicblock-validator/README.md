# magicblock-validator

Run a leader to accept application transactions, synchronize accounts with the
base chain, settle state, and run recurring tasks. It uses Engine to execute
transactions, store state, and replicate to allowed followers.

## Run a leader

Prepare a configuration using the [leader example](../../config.validator.example.toml).
Replace sample identities and provider credentials, choose storage and listener
addresses, and supply the configured program executable files.

From the workspace root:

```bash
cargo run --release --locked -p magicblock-validator -- config.toml
```

The configuration path is positional. Command-line setting overrides take
precedence over environment variables, which override the TOML file. See
[configuration](../../magicblock-config/README.md) for details, or append
`--help` for available options.

## Operating it

Fund the signing identity before starting: setup can submit base-chain
transactions to initialize and delegate fee vaults. Without a configuration
file, the leader connects to devnet; it does not start a local base chain.
Domain registration is separate: use the [operator CLI](../magicblock/README.md).

To let a verifier follow this leader, add its local public identity to
`engine.replication.allowed-followers`. An empty list allows no followers.

If a managed service exits unexpectedly, the process stops. Use its logs and
[metrics](../../magicblock-metrics/README.md) to investigate before restarting.
For a replication follower without application services, run the
[verifier](../magicblock-verifier/README.md).

See [deployment prerequisites](https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/deployment-prerequisites.md)
and [recovery](https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/recovery.md)
for multi-host operation.

[Back to workspace](../../README.md)
