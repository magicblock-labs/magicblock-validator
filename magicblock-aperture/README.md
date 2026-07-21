# Magicblock Aperture

Provides the JSON-RPC (HTTP) and Pub/Sub (WebSocket) API Server for the Magicblock validator.

## Overview

This crate serves as the primary external interface for the validator. Accounts,
transactions, blocks, submission, and live subscriptions are owned by the
engine. `magicblock-ledger-deprecated` remains read-only and is consulted only
when an engine history accessor successfully returns no data.

It provides two core services running on adjacent ports:
1.  **JSON-RPC Server (HTTP):** Handles traditional request/response RPC methods like `getAccountInfo`, `getTransaction`, and `sendTransaction`.
2.  **Pub/Sub Server (WebSocket):** Manages persistent connections for clients to subscribe to streams of data, such as `accountSubscribe` or `slotSubscribe`.

The server is a lean API layer that decodes requests, ensures required accounts
through Chainlink, and calls the engine directly.

## A Note on Naming

The name "Aperture" was chosen to reflect the crate's role as a controlled opening into the validator's core.  Much like a camera's aperture controls the flow of light, this server carefully manages the flow of information—RPC requests flowing in, and state data flowing out—without exposing the internal machinery directly.

---

## Key Components

The server's architecture is divided into logical components for handling HTTP and WebSocket traffic, all underpinned by a shared state.

### HTTP Server

-   **`HttpServer`**: The low-level server built on Hyper that accepts TCP connections and manages the HTTP 1/2 protocol.
-   **`HttpDispatcher`**: The central router for all HTTP requests. It deserializes incoming JSON, identifies the RPC method, and calls the appropriate handler function. It holds a reference to the `SharedState` to access caches and databases.

### WebSocket Server

-   **`WebsocketServer`**: Manages the initial HTTP Upgrade handshake to establish a WebSocket connection.
-   **`ConnectionHandler`**: A long-lived task that manages the entire lifecycle of a single WebSocket client connection. It is responsible for the message-reading loop, keep-alive pings, and pushing outbound notifications.
-   **`WsDispatcher`**: A stateful handler created for *each* `ConnectionHandler`. It manages the specific set of active subscriptions for a single client, handling `*Subscribe` and `*Unsubscribe` requests.

### Shared Infrastructure

-   **`SharedState`**: Holds the engine, node context, Chainlink, and the read-only deprecated-ledger fallback.
-   **Engine subscriptions**: Each WebSocket subscription forwards directly from the corresponding engine account, program, signature, log, or block receiver.
-   **`GeyserPluginManager`**: Loads plugins after both sockets bind and forwards engine block and processed-transaction streams through a bounded queue.

---

## Geyser Plugin Support

Aperture integrates with the Solana Geyser plugin system to enable extensible, real-time streaming of events. 

### Configuration

Geyser plugins are loaded from shared library files (`.so` on Linux, `.dylib` on macOS) specified in the server configuration. Each plugin requires a JSON configuration file with the following structure:

```json
{
  "libpath": "/path/to/plugin.so"
}
```

Multiple plugins can be loaded simultaneously by providing multiple configuration paths.
`event_processors` controls the number of queue workers; zero is treated as one.
The queue is bounded at 1,024 events. When no plugin loads successfully,
Aperture creates no processed-transaction subscription, avoiding Geyser fanout
and balance-clone overhead on transaction execution.

Plugin load and callback failures are logged and remain non-fatal to the RPC
server and to other plugins.

### Important Requirements

**Toolchain & Agave Version Compatibility**: Plugin binaries must be compiled with the same Rust toolchain and Agave (Solana) version as the validator and Aperture. Version mismatches can cause:

- Binary incompatibility (symbol resolution failures)
- Undefined behavior in C FFI boundaries
- Data layout misalignment in shared structures

Ensure that both your validator and any Geyser plugins are built against the same Agave version and toolchain.

### Example Plugin

Magicblock provides a customized Yellowstone gRPC plugin designed to work with the Magicblock validator. For details on building and configuring this plugin, see:

[Yellowstone gRPC Geyser Plugin](https://github.com/magicblock-labs/yellowstone-grpc/blob/mbv-6.0/yellowstone-grpc-geyser/README.md)

---

## Request Lifecycle

### HTTP Request (`sendTransaction` example)

1.  A client sends a `sendTransaction` request to the HTTP port.
2.  The `HttpServer` accepts the connection and passes the request to the `HttpDispatcher`.
3.  The `HttpDispatcher` parses the request and calls the `send_transaction` handler.
4.  The handler decodes the transaction and asks Chainlink to ensure its accounts.
5.  It calls the engine transaction accessor to execute or schedule the transaction.
6.  Unless preflight is skipped, the handler awaits the engine execution result.
7.  A JSON-RPC response containing the transaction signature is serialized and sent back to the client.

### WebSocket Subscription (`accountSubscribe` example)

1.  A client connects to the WebSocket port and initiates an HTTP Upgrade request.
2.  The `WebsocketServer` handles the handshake, and upon success, spawns a dedicated `ConnectionHandler` task for that client.
3.  The client sends an `accountSubscribe` JSON message over the WebSocket.
4.  The `ConnectionHandler` receives the message and passes it to its `WsDispatcher`.
5.  The `WsDispatcher` opens the matching engine subscription and retains its forwarding task for automatic cancellation on disconnect.
6.  A subscription ID is sent back to the client.
7.  Later, the engine publishes an account update.
8.  The subscription task encodes it and sends it to the `ConnectionHandler`'s private channel.
10. The `ConnectionHandler` receives the payload and writes it to the WebSocket stream, pushing the update to the client.

---

## Features

-   **Asynchronous & Non-blocking**: Built on Tokio and Hyper for high concurrency.
-   **Graceful Shutdown**: Utilizes `CancellationToken`s and RAII guards (`Shutdown`) to ensure the server and all active connections can terminate cleanly.
-   **Performant Lookups**: Employs a two-level caching strategy for transaction statuses and server-side filtering for `getProgramAccounts` to minimize database load.
-   **Solana API Compatibility**: Implements a large subset of the standard Solana JSON-RPC methods and subscription types.
-   **Geyser Plugin Support**: Integrates with Solana's Geyser plugin system for extensible in-validator event streaming. Dynamically loads and manages plugins that receive real-time notifications for account updates, transactions, blocks, and slot status changes.

## Engine history and metadata

`getBlock`, `getBlockTime`, `getTransaction`, `getSignatureStatuses`, and
`getSignaturesForAddress` query the engine first and propagate engine read
errors. Deprecated storage is a historical fallback only after an engine miss.
Address-signature pages merge both stores newest-first with deduplication and
cursor/limit handling. Engine block reads request only the transaction detail
needed by the RPC configuration.

Geyser transaction notifications include the available result, fee, native
balances, logs, CPI trace, return data, and compute units. Account write versions
are globally increasing. The following remain explicit placeholders because the
engine does not retain them: account transaction context, transaction index,
parent blockhash, executed-transaction count, and entry count.
