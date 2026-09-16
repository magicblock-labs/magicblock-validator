# magicblock-validator-tui

A terminal monitor for a leader's transactions, slots, and logs. It connects over
HTTP RPC and WebSocket, so it can monitor a remote validator without access to
its storage.

## Run

From the workspace root, using your leader's endpoint addresses:

```bash
cargo run --release --locked -p magicblock-validator-tui -- \
  --rpc-url http://127.0.0.1:7799 \
  --ws-url ws://127.0.0.1:7800
```

Use `--help` for additional options. Display settings do not change the remote
validator's configuration.

## Controls

- Tab and arrow keys move between views and entries.
- Enter opens transaction details.
- `q` closes a detail view or quits.

Connection failures appear in the client log stream. The monitor shows RPC
observations, not internal validator tracing.

Verifiers do not expose application RPC; use their
[metrics and logs](../magicblock-verifier/README.md) instead.

[Back to workspace](../../README.md)
