# `mbv-tui`

`bins/mbv-tui` is an external RPC/websocket monitor. It subscribes to slots
and logs and fetches block/transaction details through compatible public
endpoints.

The TUI must not own or link leader lifecycle, execution, account
materialization, or settlement behavior. It can monitor a remote leader from a
separate process or host.

Focused validation:

```bash
cargo check -p mbv-tui --locked
```
