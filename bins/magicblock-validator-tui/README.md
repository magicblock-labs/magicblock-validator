# `magicblock-validator-tui`

Standalone terminal monitor for a leader's HTTP RPC and WebSocket endpoints.
It runs out of process and can monitor a remote host without access to the
validator's local storage.

## Run

From the workspace root, using the addresses configured on the leader:

```bash
cargo run --release --locked -p magicblock-validator-tui -- \
  --rpc-url http://127.0.0.1:7799 \
  --ws-url ws://127.0.0.1:7800
```

Use `--help` for optional display metadata. Ledger path and block-time display
values do not configure or modify the remote validator.

## Data and controls

Slot subscriptions drive slot updates and HTTP `getBlock` transaction reads.
`logsSubscribe` supplies transaction logs, while `getTransaction` supplies
transaction details. These are RPC observations, not an in-process view of
validator tracing or an authoritative execution audit.

Use Tab/arrow keys to move between views, Enter to inspect a selected
transaction, and `q` to quit (or close an open detail view first).
Connection and subscription failures are shown in the client log stream.

The TUI cannot monitor a verifier through application RPC because the verifier
does not expose those services. Use the [verifier's metrics and logs][verifier]
for its operational state.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[verifier]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/bins/magicblock-verifier/README.md
