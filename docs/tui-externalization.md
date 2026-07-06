# External TUI Design

## Goal

Replicate the `feat/tui-feature` TUI without coupling it to validator internals.

## Implemented Option (Recommended): RPC/WS Client

- Standalone binary crate: `bins/mbv-tui`
- Data sources:
  - WebSocket `slotSubscribe` for slot header updates
  - HTTP RPC `getBlock` (driven by incoming slots) for transaction list (`signature`, `slot`, `success`)
  - WebSocket `logsSubscribe(All)` for transaction log stream and status summaries
  - HTTP RPC `getTransaction` for transaction detail popup
- Characteristics:
  - Works against any compatible validator endpoint
  - No validator-process embedding required
  - No internal channels or compile-time coupling to leader orchestration

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

### Independent binary (chosen)

The TUI is distributed only as `mbv-tui`:
- Clean separation of concerns
- Decoupled release cadence
- Can monitor remote validators from a separate host

## Notes on Parity

The visual layout and interaction model are preserved. The only semantic difference is the Logs tab source:
- In-process TUI: validator tracing logs
- External TUI: websocket transaction log notifications plus client connection/status logs
