# MagicBlock CLI

`magicblock` provides explicit operator commands for managing a leader's Magic
Domain record and checking a validator's RPC, execution, and PubSub paths.

## Build

```bash
cargo build -p magicblock --locked
```

The binary is written to `target/debug/magicblock`.

## Domain records

Domain commands load the leader configuration, including `MBV_` environment
overlays, and use its first HTTP remote and local authority. They submit and
confirm a Magic Domain Program transaction.

Register a record:

```bash
magicblock domain register \
  --config config.toml \
  --country-code US \
  --fqdn https://validator.example.com
```

Synchronize its mutable fields:

```bash
magicblock domain sync \
  --config config.toml \
  --country-code US \
  --fqdn https://validator.example.com
```

Remove it:

```bash
magicblock domain unregister --config config.toml
```

## Healthcheck

The healthcheck requires a validator configured with the v42 calculator
program. It derives the WebSocket endpoint from the HTTP URL using the
adjacent-port Solana convention.

```bash
magicblock healthcheck \
  --url http://127.0.0.1:8899 \
  --timeout 10s
```

Within one end-to-end deadline, the command:

1. builds one bounded randomized v42 expression containing recursive CPIs;
2. signs one transaction with a fresh keypair;
3. registers signature and target-account subscriptions before submission;
4. requires `sendTransaction` to return the locally derived signature;
5. requires successful signature notification and `getSignatureStatuses`;
6. requires an account notification before the deadline.

The account notification's value and context slot are intentionally ignored.
Success is written as one line to stdout. Structured progress and timing are
written to stderr; set `RUST_LOG` to control their verbosity.
