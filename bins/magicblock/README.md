# magicblock

Operator tools for managing a validator's domain record and checking its RPC,
execution, and subscriptions. These commands do not start or stop the validator.

## Build

From the workspace root:

```bash
cargo build --release --locked -p magicblock
```

The binary is `target/release/magicblock`. Use that path below, or add it to
your `PATH`. Run `magicblock --help` for available commands.

## Domain records

Commands use the [leader configuration](../../magicblock-config/README.md) and
its signing identity. They submit real base-chain transactions.

```bash
magicblock domain register --config config.toml \
  --country-code US --fqdn https://validator.example.com

magicblock domain sync --config config.toml \
  --country-code US --fqdn https://validator.example.com

magicblock domain unregister --config config.toml
```

Use `register` to create a record, `sync` to update it, and `unregister` to
remove it.

## Healthcheck

The validator must have the v42 calculator program configured. Use its HTTP
address; the WebSocket endpoint is derived using the next port.

```bash
magicblock healthcheck --url http://127.0.0.1:7799 --timeout 10s
```

This submits a test transaction and checks execution status and subscription
notifications within the deadline. Success is printed to stdout; progress and
errors go to stderr. Set `RUST_LOG` to adjust logging.

[Back to workspace](../../README.md)
