# External TUI Design

## Goal
Replicate the `feat/tui-feature` TUI without coupling it to validator internals.

## Implemented Option (Recommended): RPC/WS Client
- New standalone binary crate: `tools/magicblock-tui-client`
- Data sources:
  - WebSocket `slotSubscribe` for slot header updates
  - WebSocket `logsSubscribe(All)` for transaction list (`signature`, `slot`, `success`) and log stream
  - HTTP RPC `getTransaction` for transaction detail popup
- Characteristics:
  - Works against any compatible validator endpoint
  - No validator-process embedding required
  - No internal channels, no compile-time coupling to `magicblock-api`/`magicblock-processor`

## Geyser Plugin Option (Less Preferable)
Feasible, but heavier operationally:
- Pros:
  - Direct access to rich execution/block events
  - Potentially lower latency and richer event semantics
- Cons:
  - Requires plugin deployment/config and validator plugin lifecycle management
  - Tighter coupling to validator internals and plugin API stability
  - Harder distribution story for operators who just want a monitor

Given the current requirements, RPC/WS is the best default.

## Packaging Strategy

### Same binary with feature-gate
Feasible and now implemented:
- `magicblock-validator` now has a `tui` feature which launches the TUI after validator startup.
- Invocation: `cargo run --features tui --bin magicblock-validator`
- The default headless mode is unchanged when `tui` is not enabled.

### Independent binary (chosen)
Still the best fit for strict externalization:
- Clean separation of concerns
- Decoupled release cadence
- Can monitor remote validators from a separate host

## Notes on Parity
The visual layout and interaction model are preserved. The only semantic difference is the Logs tab source:
- In-process TUI: validator tracing logs
- External TUI: websocket transaction log notifications plus client connection/status logs
