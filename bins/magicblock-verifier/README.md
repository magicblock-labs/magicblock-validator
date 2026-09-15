# `magicblock-verifier`

Follower process that applies an upstream Engine replication stream using the
shared MBV runtime image. It does not start application RPC, account cloning,
settlement, task scheduling, or administrative services.

## Configure and run

From the workspace root:

```bash
cargo build --release --locked -p magicblock-verifier
cargo run --release --locked -p magicblock-verifier -- config.verifier.example.toml
```

Before running, edit the [verifier example][verifier-config]:

- supply the verifier's local signing identity and independent storage paths;
- set the upstream replication address and the upstream's actual authority;
- allow the verifier's identity in the upstream's follower configuration;
- supply program IDs and ELF artifacts matching the leader;
- use a metrics address that does not conflict with other local processes.

The TOML path is positional. `MBV_VERIFIER_` environment values override the
file; nested keys use `__`. Remote authority comes from the upstream setting,
not an independent `engine.authority.remote` override.

## Replication lifecycle

The verifier opens Engine with external block pacing and starts
`ReplicationClient`. A nonempty downstream follower allowlist also enables
a replication dispatcher, allowing a verifier to relay to other followers.

When a replicated snapshot requires reopening storage, `RestartRequired`
closes the Engine instance and starts a new one from disk. The process keeps its
metrics listener bound throughout that loop. Other managed-service termination
reasons end the process after coordinated shutdown.

`GET /metrics` exposes MBV and Engine collectors; it is not an application RPC
or a guarantee that replication is caught up. Diagnose startup and divergence
using the process logs and upstream configuration.

See [deployment inputs][deployment], [recovery][recovery], and
[replication trust][replication] for host and protocol boundaries.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[verifier-config]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/config.verifier.example.toml
[deployment]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/deployment-prerequisites.md
[recovery]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/recovery.md
[replication]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/architecture/replication-trust.md
