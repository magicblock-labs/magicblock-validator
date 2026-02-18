# magicblock-tui-client

External terminal UI for a running Magicblock validator.

## Run

```bash
cargo run -p magicblock-tui-client -- \
  --rpc-url http://127.0.0.1:8899 \
  --ws-url ws://127.0.0.1:8900
```

Or via `make` (defaults to localhost endpoints when not specified):

```bash
make tui-client
```

Override endpoints:

```bash
make tui-client TUI_RPC_URL=http://127.0.0.1:8899 TUI_WS_URL=ws://127.0.0.1:8900
```

## Validator In-Process Mode

The validator binary can also launch this TUI when built with the `tui` feature:

```bash
cargo run --features tui --bin magicblock-validator
```

Without `--features tui`, validator startup remains headless.

## Data Sources
- `slotSubscribe` (WS) for slot updates
- `logsSubscribe(All)` (WS) for transaction feed and tx logs
- `getTransaction` (RPC) for detail popup
