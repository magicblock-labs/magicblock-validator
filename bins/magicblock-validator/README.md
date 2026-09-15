# `magicblock-validator`

Leader process for an Ephemeral Rollup. It hosts Engine and the application
service graph: Chainlink, Aperture, settlement, repeated tasks, metrics, and
optional fee claims. It also serves Engine replication to allowed followers.

## Run

From the workspace root:

```bash
cargo build --release --locked -p magicblock-validator
cargo run --release --locked -p magicblock-validator -- --config config.example.toml
```

Edit the [leader example][leader-config] before starting: replace provider
credentials and identity placeholders, select storage/listener addresses, and
provide the configured program ELF files. The example is a configuration
reference, not a deployment-ready identity or artifact bundle.

CLI overrides environment, then TOML, then defaults. For example:

```bash
MBV_METRICS__ADDRESS=127.0.0.1:9090 \
cargo run --release --locked -p magicblock-validator -- --config config.example.toml
```

## Startup and shutdown

The leader opens the legacy history store, builds the shared runtime image,
loads base-chain rent, opens Engine, and wires its services. Engine owns execution
and internal block pacing; Chainlink owns base-chain synchronization.

Starting the leader can perform base-chain setup such as funding checks and
fee-vault initialization. Magic Domain registration is separate: startup and
shutdown do not register or remove a domain record. Use the [operator CLI][operator].

Unexpected managed-service exit is process-fatal. Shutdown stops synchronization
and terminates the managed services and Engine through the shutdown manager.
The process exit code reflects its shutdown reason.

The application RPC/WebSocket endpoints belong to the leader; a verifier is not
a replacement RPC endpoint. See [deployment inputs][deployment] and
[recovery][recovery] for multi-host operation.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[leader-config]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/config.example.toml
[operator]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/bins/magicblock/README.md
[deployment]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/deployment-prerequisites.md
[recovery]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/recovery.md
