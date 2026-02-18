# magicblock-tui-client

External terminal UI for a running Magicblock validator.

## Run

```bash
cargo run -p magicblock-tui-client -- \
  --rpc-url http://127.0.0.1:7799 \
  --ws-url ws://127.0.0.1:7800
```

Or via `make` (defaults to localhost endpoints when not specified):

```bash
make tui-client
```

Override endpoints:

```bash
make tui-client TUI_RPC_URL=http://127.0.0.1:7799 TUI_WS_URL=ws://127.0.0.1:7800
```

## Validator In-Process Mode

The validator binary can also launch this TUI when built with the `tui` feature:

```bash
cargo run --features tui --bin magicblock-validator
```

In this mode, TUI RPC/WS endpoints are derived from the validator `aperture.listen` config.

Without `--features tui`, validator startup remains headless.

## Data Sources
- `slotSubscribe` (WS) for slot updates
- `getBlock` (RPC, driven by slots) for transaction feed
- `logsSubscribe(All)` (WS) for tx log lines and status summaries
- `getTransaction` (RPC) for detail popup
