# `magicblock-aperture`

The leader's JSON-RPC and WebSocket server. Aperture decodes Solana-style
requests, asks Chainlink to ensure required accounts, and accesses Engine for
execution, account reads, history, and live subscriptions.

## Request boundaries

`initialize_aperture` creates the HTTP and WebSocket services over
`SharedState`. The WebSocket listener uses the port after the configured HTTP
port. Per-connection dispatchers own subscription forwarding tasks and cancel
them when the connection ends.

For transaction submission, account preparation precedes Engine submission.
The non-skipped preflight path awaits the execution result; skipped preflight
does not give the same acknowledgement. A returned signature must not be treated
as proof of base-chain settlement.

## Historical reads

Block, transaction, time, and signature-history methods read Engine first.
Engine errors propagate; only successful misses consult the read-only
[deprecated ledger][legacy]. Address-signature pagination merges available
history newest-first with deduplication and cursor/limit handling.

Legacy RocksDB reads run on blocking workers under a concurrency semaphore.
This bounds admitted synchronous reads, not disk latency or all waiting requests.
Engine block queries request the transaction detail needed by the RPC response.

## Geyser plugins

Plugins load after both listener sockets bind. Configure JSON files with a
`libpath` pointing to a shared library built against the validator's Rust and
Agave ABI. The loaded library must remain alive while its plugin objects exist.

Engine block and processed-transaction events pass through a bounded queue.
`event_processors` selects the worker count, with zero treated as one.
If no plugin loads, Aperture does not create the processed-transaction
subscription. Load and callback errors are logged without taking down the
other plugins or RPC server.

Notification data is limited to what Engine retains. Do not infer complete
Solana validator metadata from the plugin interface: some fields are
placeholders, including transaction index and parent blockhash.

Use [configuration][config] for limits and plugin settings, and
[service interfaces][interfaces] for client-facing compatibility boundaries.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[legacy]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-ledger/README.md
[config]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-config/README.md
[interfaces]: https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/service-interfaces.md
