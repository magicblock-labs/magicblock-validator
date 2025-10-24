# Magicblock Aperture

Provides the JSON-RPC (HTTP) and Pub/Sub (WebSocket) API Server for the Magicblock validator.

## Overview

This crate serves as the primary external interface for the validator, allowing clients to query the ledger, submit transactions, and subscribe to real-time events. It is a high-performance, asynchronous server built with low-level libraries for maximum control over implementation.

It provides two core services running on adjacent ports:
1.  **JSON-RPC Server (HTTP):** Handles traditional request/response RPC methods like `getAccountInfo`, `getTransaction`, and `sendTransaction`.
2.  **Pub/Sub Server (WebSocket):** Manages persistent connections for clients to subscribe to streams of data, such as `accountSubscribe` or `slotSubscribe`.

The server is designed to be a lean API layer that validates and sanitizes incoming requests before dispatching them to the `magicblock-processor` crate for heavy computation.

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

-   **`SharedState`**: The global, read-only context that is shared across all handlers. It provides `Arc`-wrapped access to the `AccountsDb`, `Ledger`, various caches, and the `DispatchEndpoints` for communicating with the processor.
-   **`EventProcessor`**: A background worker that listens for broadcasted events from the validator core (e.g., `TransactionStatus`, `AccountUpdate`) and forwards them to the appropriate WebSocket subscribers via the `SubscriptionsDb`.

---

## Request Lifecycle

### HTTP Request (`sendTransaction` example)

1.  A client sends a `sendTransaction` request to the HTTP port.
2.  The `HttpServer` accepts the connection and passes the request to the `HttpDispatcher`.
3.  The `HttpDispatcher` parses the request and calls the `send_transaction` handler.
4.  The handler decodes and sanitizes the transaction, checks for recent duplicates in the `TransactionsCache`, and performs a preflight simulation by default.
5.  If validation passes, it sends the transaction to the `magicblock-processor` via the `transaction_scheduler` channel.
6.  The handler awaits a successful execution result from the processor.
7.  A JSON-RPC response containing the transaction signature is serialized and sent back to the client.

### WebSocket Subscription (`accountSubscribe` example)

1.  A client connects to the WebSocket port and initiates an HTTP Upgrade request.
2.  The `WebsocketServer` handles the handshake, and upon success, spawns a dedicated `ConnectionHandler` task for that client.
3.  The client sends an `accountSubscribe` JSON message over the WebSocket.
4.  The `ConnectionHandler` receives the message and passes it to its `WsDispatcher`.
5.  The `WsDispatcher` registers the client's interest in the global `SubscriptionsDb`, storing a "cleanup" handle to ensure automatic unsubscription on disconnect (RAII).
6.  A subscription ID is sent back to the client.
7.  Later, the `magicblock-processor` modifies the subscribed account and broadcasts an `AccountUpdate`.
8.  The `EventProcessor` receives this update, looks up the account in `SubscriptionsDb`, and finds the client's channel.
9.  It sends a formatted notification payload to the `ConnectionHandler`'s private channel.
10. The `ConnectionHandler` receives the payload and writes it to the WebSocket stream, pushing the update to the client.

---

## Features

-   **Asynchronous & Non-blocking**: Built on Tokio and Hyper for high concurrency.
-   **Graceful Shutdown**: Utilizes `CancellationToken`s and RAII guards (`Shutdown`) to ensure the server and all active connections can terminate cleanly.
-   **Performant Lookups**: Employs a two-level caching strategy for transaction statuses and server-side filtering for `getProgramAccounts` to minimize database load.
-   **Solana API Compatibility**: Implements a large subset of the standard Solana JSON-RPC methods and subscription types.

