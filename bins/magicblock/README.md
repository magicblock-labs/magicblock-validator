# magicblock

Operator tools for claiming validator fees, managing a validator's domain record,
and checking its RPC, execution, and subscriptions. These commands do not start
or stop the validator or Engine.

## Build

From the workspace root:

```bash
cargo build --release --locked -p magicblock
```

The binary is `target/release/magicblock`. Use that path below, or add it to
your `PATH`. Run `magicblock --help` for available commands.

## Fee claims

Claim accrued fees once using the base-chain RPC and signing identity from the
[leader configuration](../../magicblock-config/README.md):

```bash
magicblock claim-fees --config /etc/magicblock/config.toml
```

The command sends and confirms a real base-chain transaction, paying fees from
the configured identity and receiving the claim at that same identity. Vault
balances at or below 100,000,000 lamports are skipped successfully. Structured
info logs on stderr report the confirmed signature or skip reason; `RUST_LOG`
controls verbosity. Configuration, signing, and RPC failures exit nonzero with
an error on stderr.

### Migration and scheduling

The validator no longer claims fees at startup or periodically. Remove the
obsolete `[admin]` section and `MBV_ADMIN__*` environment overrides; they are no
longer accepted. Fee-vault setup remains part of validator startup.

For recurring claims, invoke the one-shot command from cron or a systemd timer.
For example, this cron entry runs daily at midnight:

```cron
0 0 * * * /usr/local/bin/magicblock claim-fees --config /etc/magicblock/config.toml
```

Use the intended service user and environment, including any `MBV_` overrides,
and capture stdout/stderr in your scheduler's logs. There is no CLI interval loop.

## Domain records

Commands use the [leader configuration](../../magicblock-config/README.md) and
its signing identity. They submit real base-chain transactions. Operator commands
require the exact file supplied with `--config`; missing files fail rather than
falling back to defaults or searching parent directories.

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
