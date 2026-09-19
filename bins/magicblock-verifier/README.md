# magicblock-verifier

Follow a leader and replay its transaction stream with the same runtime programs.
A verifier does not serve applications: RPC, base-chain account synchronization,
settlement, and recurring tasks run on the leader.

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

When applying a replicated snapshot requires a restart, the verifier drains and
reopens Engine automatically. Metrics remain available during that restart.

To relay the stream to other followers, the verifier must hold the upstream
authority's private key and use it as its local signer. A verifier with a distinct
local identity can follow, but Engine disables its replication dispatcher even
when downstream followers are allowed.

See [recovery guidance](https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/recovery.md)
for operational recovery procedures.

[Back to workspace](../../README.md)
