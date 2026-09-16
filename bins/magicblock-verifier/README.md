# magicblock-verifier

Runs a follower that replays an upstream Engine replication stream using the
same runtime programs as the leader. It does not expose application RPC or run
account synchronization, settlement, or recurring tasks.

## Run a verifier

Prepare a configuration using the [verifier example](../../config.verifier.example.toml):

- Give it a local signing identity and independent storage.
- Set the upstream address and authority, and allow the verifier's identity on
  the upstream.
- Supply the same program IDs and executable files as the leader.
- Choose a metrics address that does not conflict with other processes.

From the workspace root:

```bash
cargo run --release --locked -p magicblock-verifier -- config.verifier.toml
```

The configuration path is positional. Use `MBV_VERIFIER_` environment variables
for overrides; see [configuration](../../magicblock-config/README.md).

## Monitoring and recovery

Monitor the process through logs and [metrics](../../magicblock-metrics/README.md),
not the application TUI. A reachable metrics endpoint does not mean replication
is caught up.

The verifier reopens Engine when a replicated snapshot requires it. It can also
relay replication to configured downstream followers.

See [recovery guidance](https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/recovery.md)
for operational recovery procedures.

[Back to workspace](../../README.md)
