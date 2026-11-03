# magicblock

Operator tools for claiming validator fees, managing a validator's domain record,
checking its RPC and execution paths, and monitoring transaction failures. These
commands do not start or stop the validator or Engine.

Outcomes and diagnostics are logged to stderr. `RUST_LOG` controls verbosity;
`RUST_LOG=magicblock=debug` enables healthcheck stages and full failure diagnostics.

## Build

From the workspace root:

```bash
cargo build --release --locked -p magicblock
```

The binary is `target/release/magicblock`. Use that path below, or add it to
your `PATH`. Run `magicblock --help` for available commands.

## Fee claims

Claim accrued fees once using the base-chain RPC and signing identity from the
[leader configuration](../../config/README.md):

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

Commands use the [leader configuration](../../config/README.md) and
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

`sync` submits all mutable fields from a freshly built record. Fields not
exposed as command options use the CLI's defaults rather than preserving their
on-chain values. Each operation logs its confirmed transaction signature;
existence and authority checks are enforced by the domain program.

The [pinned sync handler](https://github.com/magicblock-labs/magic-domain-program/blob/64c7c5e35871b07dffdabbe20f6ca22a7f9dd55e/src/processors/sync.rs)
does not apply country-code changes; registration sets that value.

## Healthcheck

The validator must have the v42 calculator program configured. Use its HTTP
address; the WebSocket endpoint is derived using the next port.

```bash
magicblock healthcheck --url http://127.0.0.1:7799 --timeout 10s
```

This submits a test transaction and checks execution status and subscription
notifications within the deadline. A successful check logs the transaction
signature; a failed check exits nonzero with the failing stage and cause.

## Monitor failures

Watch live transaction failures without submitting a probe:

```bash
magicblock monitor --url http://127.0.0.1:7799
```

The command accepts HTTP or WebSocket URLs and logs each failure at warn level,
with its signature, execution slot, and error. Debug logs include account/program
addresses, instructions (with Base64 data), and available execution logs.
Connecting and subscribing must complete within 10 seconds; monitoring itself
has no deadline. Ctrl-C stops monitoring successfully.

Events report Engine loading/execution failures with their execution slot.
Chainlink failures, simulation, replay, and admission rejections are not included.
Instruction account indices address `resolvedAccountKeys` when available,
otherwise `staticAccountKeys`; lookup-table addresses may be unresolved for
loading failures.

Delivery is best-effort, bounded, and live-only, with no replay or loss reporting.
Slow subscriptions can stop receiving notifications. WebSocket disconnection
exits nonzero. Successful transactions do not produce events, so an idle monitor
is not proof that the validator is healthy. Use `healthcheck` for an active probe.

[Back to workspace](../../README.md)
