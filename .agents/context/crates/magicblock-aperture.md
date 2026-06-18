# `magicblock-aperture`

## Purpose

`magicblock-aperture` owns the validator's external Solana-compatible ingress and event egress surface. It exposes JSON-RPC over HTTP, PubSub over WebSocket, local request caches, and dynamic Agave Geyser plugin notifications. `magicblock-api` constructs it during validator startup through `initialize_aperture`, then runs the returned `JsonRpcServer` as part of the validator service graph.

High-level responsibilities:

- bind the HTTP JSON-RPC listener and adjacent WebSocket PubSub listener;
- parse, validate, route, and encode supported Solana JSON-RPC requests plus Magic Router compatibility methods;
- serve local account, ledger, block, transaction, node, token, and mocked Solana RPC reads;
- submit and simulate transactions through `magicblock-core` dispatch channels and the processor scheduler;
- trigger Chainlink/account-cloner ensure paths for RPC reads and transactions when the validator is primary;
- maintain short-lived transaction and blockhash caches used for replay prevention, status reads, and blockhash validity;
- maintain WebSocket subscription registries and push account, program, signature, logs, and slot notifications;
- load and notify Agave Geyser plugins from configured JSON files.

Aperture sits directly on performance-sensitive RPC, PubSub, event-processing, account-sync, and transaction-submission paths. Keep per-request and per-event work lean: avoid blocking I/O, unbounded allocations, high-cardinality metrics labels, duplicate account fetches, and slow plugin work in hot paths. Aperture is an ingress/router layer, not the protocol source of truth for execution validity, delegation lifecycle, or settlement.

## Update requirement

Update this document in the same change whenever `magicblock-aperture` behavior, public APIs, request/response compatibility, configuration contract, event flow, cache behavior, Geyser integration, metrics, tests, or performance characteristics change. Update it for changes to:

- `initialize_aperture`, `JsonRpcServer`, `SharedState`, `NodeContext`, exported modules, or startup/shutdown ordering;
- HTTP or WebSocket method inventories in `JsonRpcHttpMethod` / `JsonRpcWsMethod`;
- RPC handler semantics, request validation, response encoding, JSON-RPC error codes, or HTTP status mapping;
- transaction preparation, replay prevention, blockhash checks, primary/replica gating, account ensuring, submission, or simulation;
- `TransactionsCache`, `BlocksCache`, `ExpiringCache`, subscription DBs, signature expiration, or notification encoders;
- Geyser plugin config keys, loading lifecycle, event payload fields, notification ordering, or error policy;
- `[aperture]` config keys in `magicblock-config`, `config.example.toml`, or CLI/env support;
- metrics names/labels from `magicblock-metrics` used by this crate;
- unit, integration, or manual validation commands for RPC, PubSub, Geyser, or performance-sensitive paths.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-aperture/Cargo.toml` | Crate manifest. Depends on account cloner, accounts DB, Chainlink, config, core, ledger, metrics, version, Hyper, Tokio, fastwebsockets, Agave Geyser, and Solana RPC/status types. |
| `magicblock-aperture/README.md` | Human-facing overview of HTTP, WebSocket, Geyser plugin support, and request lifecycle. Keep it aligned with this guide when operator-facing behavior changes. |
| `magicblock-aperture/src/lib.rs` | Public crate entrypoint. Exports `initialize_aperture`, `JsonRpcServer`, `error`, `server`, and `state`; binds sockets before starting event processors. |
| `magicblock-aperture/src/server/http/mod.rs` | Hyper/Tokio HTTP accept loop and per-connection task spawning. Stops accepting immediately on cancellation. |
| `magicblock-aperture/src/server/http/dispatch.rs` | Central HTTP JSON-RPC dispatcher, batch handling, CORS, `/health/primary`, metrics, and method routing. |
| `magicblock-aperture/src/server/websocket/mod.rs` | WebSocket listener, HTTP upgrade handling, and per-connection task spawning. |
| `magicblock-aperture/src/server/websocket/connection.rs` | Per-client WebSocket loop: request reads, responses, subscription updates, ping/inactivity handling, and shutdown. |
| `magicblock-aperture/src/server/websocket/dispatch.rs` | Per-connection subscription dispatcher and unsubscribe guard ownership. |
| `magicblock-aperture/src/requests/mod.rs` | JSON-RPC request shapes, supported HTTP/WebSocket method enums, method string mapping, and `parse_params!`. |
| `magicblock-aperture/src/requests/http/*.rs` | Individual HTTP RPC handlers and shared helpers for parsing bodies, reading/ensuring accounts, transaction preparation, and account ensuring. |
| `magicblock-aperture/src/requests/http/transaction_validation.rs` | Additional transaction-shape guardrails: rejects v0 address lookup tables and program ID indices outside the runtime packet-derived limit. |
| `magicblock-aperture/src/requests/websocket/*.rs` | Individual WebSocket subscribe handlers for account, program, signature, logs, and slot subscriptions. |
| `magicblock-aperture/src/state/*.rs` | `SharedState`, `NodeContext`, caches, subscription registries, and signature expiration. |
| `magicblock-aperture/src/encoder.rs` | Notification encoders for account, program, signature, logs, and slot PubSub payloads. |
| `magicblock-aperture/src/geyser.rs` | Dynamic Agave Geyser plugin manager and conversion from validator events to `Replica*Info*` types. |
| `magicblock-aperture/src/error.rs` | `ApertureError`, JSON-RPC `RpcError`, Solana transaction error mapping, and HTTP status override for shutdown-like failures. |
| `magicblock-aperture/tests/*.rs` | Crate-level RPC/PubSub tests using a live `JsonRpcServer` and `solana_rpc_client` / `solana_pubsub_client`. |
| `magicblock-api/src/magic_validator.rs` | Runtime consumer: builds `SharedState`, calls `initialize_aperture`, and runs the RPC server. |
| `magicblock-config/src/config/aperture.rs` | Defines `ApertureConfig` (`listen`, `event_processors`, `geyser_plugins`). |
| `config.example.toml` | Operator-facing `[aperture]` example, including `geyser-plugins`. |
| `test-integration/test-pubsub/` and `test-integration/test-magicblock-api/` | Integration suites that exercise validator-level PubSub and MagicBlock API behavior. |

Main consumers:

- `magicblock-api` is the runtime consumer and owns service orchestration around Aperture.
- External Solana/RPC clients consume the HTTP and WebSocket APIs.
- Dynamically loaded shared libraries implementing `agave_geyser_plugin_interface::GeyserPlugin` consume Geyser notifications.
- `test-kit` and `magicblock-aperture/tests/setup.rs` provide test backends for crate-local RPC tests.

Important upstream/downstream relationships:

- Upstream state comes from `AccountsDb`, `Ledger`, `Chainlink`, `DispatchEndpoints`, and `magicblock-config`.
- Transaction submission/simulation flows downstream into the processor scheduler via `TransactionSchedulerHandle`.
- Account read and transaction account availability flows downstream into `magicblock-chainlink` / `magicblock-account-cloner` ensure APIs.
- Event egress consumes `magicblock-core` account/transaction/block channels and feeds WebSocket subscriptions, local caches, and Geyser plugins.

## Public API shape / Main public types and APIs

Public surface from `src/lib.rs`:

- `initialize_aperture(config, state, dispatch, cancel) -> ApertureResult<JsonRpcServer>` — binds HTTP and WebSocket listeners, constructs the server, then starts `EventProcessor` workers. Event processors start only after sockets bind successfully so startup retries do not leak background tasks.
- `JsonRpcServer` — returned service handle with:
  - `http_addr()` and `ws_addr()` for bound listener addresses;
  - `run(self)` to concurrently run HTTP and WebSocket servers until cancellation.
- `error` module — exports `ApertureError` and public `RpcError` type used in responses and by `magicblock-api` error wrapping.
- `state` module — exports `SharedState`, `NodeContext`, `ChainlinkImpl`, and `InnerChainlinkImpl` for construction by `magicblock-api` and tests.
- `server` module — public module for lower-level server types, though most internals remain `pub(crate)`.

Key configuration/API contracts:

- `ApertureConfig::listen` binds the HTTP listener. The WebSocket listener uses `listen.port() + 1`; if HTTP port is `0`, WebSocket also binds port `0` and the actual bound ports are available from `JsonRpcServer`.
- `ApertureConfig::event_processors` controls the number of event-processing Tokio tasks.
- `ApertureConfig::geyser_plugins` is a list of JSON config paths. Each JSON file must contain `libpath` pointing to a shared library exporting `_create_plugin`.
- `NodeContext` carries validator identity, base fee, feature set, and block time. `SharedState::new` uses block time to compute blockhash validity and initializes transaction/block/subscription caches.

Supported HTTP method enum (`JsonRpcHttpMethod`) includes standard reads, transaction methods, token helpers, and Magic Router compatibility methods:

- transaction methods: `sendTransaction`, `simulateTransaction`;
- local/ledger reads: `getAccountInfo`, `getMultipleAccounts`, `getBalance`, `getBlock`, `getBlocks`, `getBlockHeight`, `getBlockTime`, `getLatestBlockhash`, `isBlockhashValid`, `getTransaction`, `getSignatureStatuses`, `getSignaturesForAddress`, `getSlot`, `getVersion`, and related Solana methods;
- token helpers: `getTokenAccountBalance`, `getTokenAccountsByDelegate`, `getTokenAccountsByOwner`;
- mocked compatibility methods such as `getHealth`, `getGenesisHash`, `getEpochInfo`, and others under `mocked.rs`;
- Magic Router compatibility: `getRoutes`, `getBlockhashForAccounts` as an alias of `getLatestBlockhash`, and `getDelegationStatus`.

Supported WebSocket method enum (`JsonRpcWsMethod`) includes:

- `accountSubscribe` / `accountUnsubscribe`;
- `programSubscribe` / `programUnsubscribe`;
- `signatureSubscribe` / `signatureUnsubscribe`;
- `logsSubscribe` / `logsUnsubscribe`;
- `slotSubscribe` / `slotUnsubscribe`;
- `ping`.

Important internal service handles:

- `HttpDispatcher` is the shared per-request context for HTTP handlers. It owns cloned handles to node context, accounts DB, ledger, Chainlink, transaction/block caches, and transaction scheduler.
- `WsDispatcher` is per WebSocket connection. It owns that client's cleanup guards and signature expirer while sharing global subscription DB and transaction cache.
- `EventProcessor` is a background worker. Each worker subscribes to account, transaction, and block event channels and forwards events to subscriptions, Geyser plugins, and caches.
- `GeyserPluginManager` owns loaded plugin trait objects and `Library` handles. The library handles must outlive plugin objects.

## Runtime flows

### Startup and shutdown flow

```text
magicblock-api
  -> SharedState::new(...)
  -> initialize_aperture(config, state, dispatch, cancel)
      -> bind HTTP listener
      -> derive and bind WebSocket listener
      -> construct WebsocketServer and HttpServer
      -> unsafe GeyserPluginManager::new(config.geyser_plugins)
      -> spawn config.event_processors EventProcessor tasks
  -> JsonRpcServer::run()
      -> join HTTP and WebSocket accept loops
```

Important details:

1. Socket binding happens before event processors start. Preserve this ordering to avoid leaked event tasks after bind failures in tests/startup retries.
2. HTTP and WebSocket accept loops stop accepting new connections when `cancel` is triggered.
3. Active HTTP/WebSocket connection tasks are not drained indefinitely; shutdown favors fast validator restart.
4. Geyser plugins are loaded during `EventProcessor::start`; plugin loading failures fail Aperture initialization.
5. Dropping `GeyserPluginManager` calls `plugin.on_unload()` for each loaded plugin.

### HTTP request dispatch flow

```text
TCP connection
  -> Hyper HttpServer
  -> HttpDispatcher::dispatch
  -> extract_bytes (1 MiB cap)
  -> parse_body (single or batch)
  -> method-specific handler
  -> ResponsePayload / ResponseErrorPayload
```

Important details:

1. `OPTIONS` receives CORS headers without JSON-RPC parsing.
2. `/health/primary` returns `503 Service Unavailable` unless `CoordinationMode::current() == Primary`.
3. Request bodies are capped at 1 MiB. `Data::SingleChunk` avoids allocating for common single-chunk requests; only multi-chunk bodies allocate a `Vec`.
4. Batch requests run handlers through `FuturesOrdered`, preserving response order while allowing concurrent futures.
5. `RPC_REQUESTS_COUNT` and `RPC_REQUEST_HANDLING_TIME` are labeled by bounded method names from the enum; do not replace them with user-controlled labels.
6. JSON-RPC errors usually render with HTTP 200; shutdown-like `TransactionError::ClusterMaintenance` maps to HTTP 503 so retry-aware proxies can absorb restart gaps.

### Account read ensure flow

```text
getAccountInfo / getMultipleAccounts / simulation account reads
  -> CoordinationMode check
  -> Chainlink ensure_accounts when primary
  -> AccountsDb get_account
  -> LockedAccount race-free encode
  -> JSON-RPC response with BlocksCache slot context
```

Important details:

1. In replica/non-primary modes, read helpers skip on-chain interactions and return local `AccountsDb` state only.
2. `getAccountInfo` and `getMultipleAccounts` pass `mark_empty_if_not_found` so missing accounts can be represented locally, then render synthetic empty placeholder system accounts as JSON-RPC `null`.
3. Ensure failures for account reads are logged and the handler returns whatever is currently in `AccountsDb`; transaction account ensure failures are stricter.
4. Encoding uses `LockedAccount` to avoid races while reading account data.
5. `getMultipleAccounts` does **not** enforce agave's 100-pubkey-per-request limit. The handler in `requests/http/get_multiple_accounts.rs` processes every pubkey in the input array with no count cap; the only bound is the global 1 MiB request-body limit (see HTTP flow detail 3). Clients relying on agave's rejection of >100 keys will not get that error here.

### Transaction submission flow

```text
sendTransaction
  -> require primary
  -> decode base58/base64 transaction
  -> validate_supported_transaction_shape
  -> validate recent blockhash against BlocksCache
  -> sanitize and verify signatures
  -> reserve signature in TransactionsCache
  -> Chainlink ensure_transaction_accounts
  -> scheduler.execute (preflight) OR scheduler.schedule (skipPreflight)
  -> return signature
```

Important details:

1. `sendTransaction` and `simulateTransaction` are primary-only. Preserve this gating; replicas must not perform on-chain account ensures or schedule execution.
2. Replay prevention reserves the signature in `TransactionsCache` before account ensuring and scheduling. Duplicate signatures return `AlreadyProcessed`.
3. `TransactionsCache` TTL is 75 seconds, intentionally longer than the 60-second blockhash window to avoid replay after premature cache eviction.
4. v0 transactions with address lookup tables are currently rejected.
5. Program ID indices are limited to `1232 / size_of::<Pubkey>()` because downstream runtime compute-budget code assumes packet-bounded program indices.
6. `skip_preflight = true` schedules fire-and-forget and increments `TRANSACTION_SKIP_PREFLIGHT`; otherwise the handler awaits scheduler execution.

### Simulation flow

```text
simulateTransaction
  -> require primary
  -> decode and validate transaction
  -> optionally replace recent blockhash
  -> Chainlink ensure_transaction_accounts
  -> scheduler.simulate
  -> optionally merge requested post-simulation account states
  -> encode RpcSimulateTransactionResult
```

Important details:

1. `replace_recent_blockhash` mutates the transaction before sanitization and can return `replacement_blockhash` in the response.
2. Requested account snapshots reject binary/base58 encodings and too many requested accounts.
3. If simulation fails, requested account snapshots are returned as `None` entries.
4. Current response leaves some Solana fields as `None`, including fee, loaded address data, balances, and loaded accounts data size.

### WebSocket subscription flow

```text
WebSocket TCP connection
  -> HTTP upgrade
  -> ConnectionHandler task
  -> WsDispatcher per connection
  -> subscribe handler registers global subscriber
  -> CleanUp guard stored in connection unsubs map
  -> EventProcessor sends encoded notification to connection channel
  -> ConnectionHandler writes text frame
```

Important details:

1. Each connection has an MPSC channel with capacity `4096` for outbound updates.
2. Connection IDs are generated with a relaxed atomic counter.
3. `CleanUp` guards provide RAII unsubscription. Dropping/removing the guard removes the subscriber from global registries.
4. `signatureSubscribe` is one-shot. It is removed atomically on notification and also tracked by a per-connection `SignaturesExpirer` with a 90-second TTL checked every 5 seconds.
5. The connection loop sends WebSocket pings every 30 seconds and closes connections inactive for more than 60 seconds.
6. `WsDispatcher::drop` drains pending signature subscriptions to avoid orphaned global entries.

### Event processor and Geyser flow

```text
processor/ledger dispatch channels
  -> EventProcessor workers
      -> WebSocket subscription DB notifications
      -> GeyserPluginManager notifications
      -> TransactionsCache / BlocksCache updates
```

Event ordering in the current implementation:

1. Block update: send slot WebSocket notification, notify Geyser slot, notify Geyser block, then update `BlocksCache`.
2. Account update: send account WebSocket notification, send program WebSocket notification, then notify Geyser account.
3. Transaction status: send signature WebSocket notification, send logs WebSocket notification, notify Geyser transaction, then push final status into `TransactionsCache`.
4. Individual Geyser notification errors are logged with `warn!` and do not stop event processing.

Geyser caveats:

- Plugin callbacks run inline on event-processor tasks. Slow or blocking plugins can delay WebSocket notifications and cache updates handled by that task.
- Account Geyser notifications set `txn: None` and `write_version: 0`.
- Slot notifications use `SlotStatus::Rooted` and parent `slot.checked_sub(1)`.
- Block metadata currently uses placeholder `parent_blockhash`, `executed_transaction_count`, and `entry_count` values.
- Plugin JSON uses `libpath`; error text in `geyser.rs` still mentions `path` in some messages, but the implemented compatibility contract is `libpath`.

## Important internals and caveats

### Coordination mode boundaries

Aperture consults `CoordinationMode::current()` to decide whether on-chain interactions are allowed. Primary mode can ensure accounts and submit/simulate transactions. Replica mode should serve local reads only and reject transaction-affecting RPC methods. Do not bypass `require_primary_rpc_method` or `needs_onchain_interactions` when adding write-like or fetch-amplifying RPC methods.

### Cache semantics

- `ExpiringCache` evicts lazily only on `push`; `get` and `contains` do not remove expired entries.
- Updating an existing key in `ExpiringCache` does not renew its lifetime because expiration records are only queued for new keys.
- `BlocksCache` stores one latest block through `ArcSwapAny` plus a 60-second blockhash cache. `block_validity` is scaled by `SOLANA_BLOCK_TIME / NodeContext::blocktime` and `MAX_VALID_BLOCKHASH_SLOTS`.
- `SharedState::new` panics if `NodeContext::blocktime` is zero through `BlocksCache::new`.
- `TransactionsCache` stores `None` for reserved/in-flight signatures and `Some(SignatureResult)` after status events.

### Subscriptions and encoders

Subscription grouping depends on encoders implementing stable ordering/equality. `AccountEncoder` includes encoding and data slice; `ProgramAccountEncoder` includes filters. Changing these comparisons can merge or split subscription groups and affects memory use and notification fanout.

`LogsSubscribe` supports all logs or `mentions(pubkey)` filtering by transaction account keys. Program subscriptions filter account data in `ProgramAccountEncoder::encode`; non-matching updates return `None` and are skipped.

### JSON and Solana compatibility

The crate intentionally implements a subset of Solana JSON-RPC behavior plus MagicBlock-specific methods. Some methods are mocked for compatibility. Do not silently change method names, response context slots, JSON-RPC error codes, or unsupported-encoding behavior without updating tests and operator/client documentation.

### Geyser FFI safety

`GeyserPluginManager::new` is unsafe because plugins cross a Rust trait-object FFI boundary. Plugins must be ABI-compatible with the validator's Agave/Solana and Rust toolchain versions. The manager stores `Library` handles beside plugin boxes so loaded symbols remain valid while plugins are used; preserve that lifetime relationship.

## Important invariants

1. HTTP listener binding must succeed before event processors are spawned.
2. The WebSocket listener must bind to `HTTP port + 1`, except when HTTP port is `0`, where both listeners request OS-assigned ports.
3. Aperture must remain a lean ingress/egress layer; it must not duplicate SVM execution, delegation lifecycle, commit, or settlement protocol logic.
4. Primary/replica gating must prevent replicas from submitting/simulating transactions or performing on-chain account ensure work.
5. `sendTransaction` must reserve signatures before scheduling to preserve replay protection.
6. `TransactionsCache` TTL must remain longer than the blockhash validity window unless replay protection is redesigned.
7. Transaction encoded bytes must be preserved for execution/replication when `replace_blockhash = false`.
8. v0 address lookup table rejection and program-index guardrails must remain aligned with runtime support.
9. Account read handlers must render synthetic empty placeholder system accounts as JSON-RPC `null`.
10. Request and metric labels must come from bounded method/config values, not client-controlled arbitrary strings.
11. Subscription cleanup guards must be retained for the life of a WebSocket subscription and dropped on unsubscribe/disconnect.
12. `signatureSubscribe` must remain one-shot and bounded by expiration to avoid unbounded memory growth.
13. Geyser plugin libraries must outlive plugin trait objects, and notification failures must not accidentally kill event processors unless the availability policy is intentionally changed.
14. Avoid adding blocking I/O, slow locks, unbounded serialization, or excessive cloning to HTTP dispatch, account ensure, transaction submission, event processing, or WebSocket notification hot paths.

## Common change areas and what to inspect

### Adding or changing an HTTP RPC method

Inspect first:

- `magicblock-aperture/src/requests/mod.rs` for enum and `as_str()` entries;
- `magicblock-aperture/src/server/http/dispatch.rs` for routing and metrics;
- matching `magicblock-aperture/src/requests/http/<method>.rs` handler;
- `magicblock-aperture/tests/` and relevant integration suites;
- `.agents/specs/validator-specification.md` for protocol-level methods like delegation, routing, or commits.

Risks:

- missing method string mapping breaks metrics and routing;
- handler may need primary-mode gating or must avoid on-chain fetches in replica mode;
- account reads may require Chainlink ensure but should avoid fetch amplification;
- response context slot and error shape are client-visible compatibility contracts.

### Changing transaction submission or simulation

Inspect first:

- `send_transaction.rs`, `simulate_transaction.rs`, `requests/http/mod.rs`, and `transaction_validation.rs`;
- `magicblock-core` transaction dispatch types and scheduler handle behavior;
- `magicblock-chainlink` account ensure behavior;
- `magicblock-aperture/tests/transactions.rs` and `transaction_primary_mode.rs`.

Risks:

- weakening replay protection;
- scheduling in replica mode;
- losing encoded transaction bytes needed by replication/execution paths;
- accepting transaction shapes the runtime cannot safely process;
- turning account ensure soft failures into silent execution of unavailable state.

### Changing account or token read methods

Inspect first:

- `get_account_info.rs`, `get_multiple_accounts.rs`, `get_balance.rs`, `get_program_accounts.rs`, token handler files, and shared read helpers;
- `magicblock-chainlink` ensure APIs and `AccountFetchOrigin` metrics;
- `magicblock-aperture/tests/accounts.rs`.

Risks:

- returning placeholder accounts instead of JSON-RPC `null`;
- breaking result ordering for multi-account reads;
- adding expensive scans or remote fetches to hot read paths;
- changing SPL token layout offsets without tests.

### Changing WebSocket/PubSub behavior

Inspect first:

- `server/websocket/connection.rs`, `server/websocket/dispatch.rs`, `requests/websocket/*.rs`;
- `state/subscriptions.rs`, `state/signatures.rs`, and `encoder.rs`;
- `magicblock-aperture/tests/websocket.rs` and `test-integration/test-pubsub/`.

Risks:

- orphaning subscriptions on disconnect;
- unbounded per-connection queues or signature subscription memory;
- changing notification JSON shape or subscription IDs;
- slow clients causing backpressure on event-processing paths.

### Changing event processing or caches

Inspect first:

- `processor.rs`, `state/blocks.rs`, `state/transactions.rs`, `state/cache.rs`;
- `magicblock-ledger` latest-block watch behavior;
- transaction status event producers in `magicblock-processor`.

Risks:

- reordering cache updates relative to client notifications;
- invalid blockhash acceptance/rejection due to blocktime or TTL changes;
- removing lazy eviction assumptions;
- increasing event processor contention with additional shared locks.

### Changing Geyser plugin support

Inspect first:

- `geyser.rs`, `processor.rs`, `magicblock-config/src/config/aperture.rs`, `config.example.toml`;
- Agave `Replica*Info*` version changes in dependency updates.

Risks:

- ABI/version mismatch and memory unsafety;
- changing operator config from `libpath`;
- blocking event processors inside plugin callbacks;
- changing placeholder event fields that downstream plugins may already consume.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-aperture`.
- Relevant focused test areas: crate tests for accounts, transactions, transaction primary-mode gating, WebSocket behavior, and transaction validation.
- Relevant integration suites: `test-magicblock-api` and `test-pubsub`; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Performance validation intent: RPC/account-read changes should report added account fetches, blocking work, serialization, or scans; transaction changes should report scheduler/account-ensure latency and replay-protection impact; PubSub/Geyser/event changes should report notification throughput, queue growth, plugin callback latency, and cache-update ordering.

## Adjacent implementation references

- `.agents/context/crates/magicblock-account-cloner.md` — account/program clone behavior reached indirectly through Chainlink.
- `.agents/context/crates/magicblock-chainlink.md` — account synchronization and ensure behavior used by RPC reads/transactions.
- `magicblock-aperture/README.md` — human-facing crate overview.
- `magicblock-config/src/config/aperture.rs` and `config.example.toml` — operator-facing Aperture configuration.
- `test-integration/test-magicblock-api/` and `test-integration/test-pubsub/` — validator-level RPC/PubSub integration suites.
