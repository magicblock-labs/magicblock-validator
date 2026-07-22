# `magicblock-rpc-client`

## Purpose

`magicblock-rpc-client` is the validator's shared async wrapper around Solana's nonblocking `RpcClient` for base-layer reads, transaction submission, and transaction confirmation. It sits on the base-layer settlement path used by the committor service and `magicblock-table-mania`, and on supporting account-fetch/admin flows used by account cloning, task-info fetching, and validator registration helpers.

High-level responsibilities:

- wrap an existing `Arc<solana_rpc_client::nonblocking::rpc_client::RpcClient>` in a cheap-to-clone `MagicblockRpcClient`;
- send base-layer transactions with MagicBlock defaults (`skip_preflight: true`, base64 encoding) and optional processed/committed confirmation;
- coalesce concurrent signature-status polling and optionally use `signatureSubscribe` websocket notifications before falling back to batched polling;
- cache recent blockhashes and slots briefly to reduce base-layer RPC load in high-volume settlement flows;
- batch `getMultipleAccounts` requests so callers do not exceed RPC provider input limits;
- expose address lookup table account helpers used by `magicblock-table-mania`;
- expose retry/error-mapping helpers in `utils` so committor callers can map Solana transaction errors into domain errors while preserving retry policy.

This crate is performance-sensitive because it is used while preparing and delivering base-layer commit, undelegation, action, and lookup-table transactions. Changes must avoid increasing confirmation latency, duplicate RPC calls, unbounded task spawning, lock contention in the shared confirmation state, or RPC-provider amplification.

End-to-end commit/undelegation semantics live in .agents/specs/validator-specification.md; this crate owns base-layer RPC reads, transaction send/confirmation, batching, caching, and retry/error mapping for settlement callers.

## Update requirement

Update this guide in the same change whenever behavior or contracts in `magicblock-rpc-client` change. In particular, update it for changes to:

- `MagicblockRpcClient` constructors, cached blockhash/slot behavior, commitment handling, or `chain_slot` observation;
- `SEND_TRANSACTION_CONFIG`, send/confirm defaults, timeout intervals, or `MagicBlockSendTransactionConfig` semantics;
- signature confirmation behavior in `src/signature_confirmer.rs`, including websocket fallback, polling batch size, cache TTL, waiter coalescing, metrics, or cancellation behavior;
- `get_multiple_accounts*`, lookup-table helpers, transaction log/CU helpers, or account-not-found handling;
- retry and error-mapping traits/functions in `src/utils.rs`;
- metrics emitted through `magicblock-metrics` for RPC-client confirmation paths;
- validation commands or integration suites relevant to base-layer send/confirm behavior.


For the general documentation-update rule, see .agents/memory/agent-memory-and-docs.md.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-rpc-client/Cargo.toml` | Package metadata and dependencies on Solana RPC/pubsub/status crates plus `magicblock-metrics`. |
| `magicblock-rpc-client/src/lib.rs` | Public crate surface, error/result types, send configuration/outcome types, `MagicblockRpcClient`, blockhash/slot caches, account/lookup-table helpers, transaction send/confirm methods, and transaction log/CU helpers. |
| `magicblock-rpc-client/src/signature_confirmer.rs` | Internal confirmation engine. Coalesces waiters, batches `getSignatureStatuses`, caches completed statuses, optionally uses websocket `signatureSubscribe`, and records RPC-client metrics. Unit tests live in this file. |
| `magicblock-rpc-client/src/utils.rs` | Public retry and error-mapping utilities used by committor send paths. |
| `magicblock-metrics/src/metrics/mod.rs` | Defines RPC-client confirmation counters: websocket subscribe/notification/fallback counts and signature-status batch counters. |
| `magicblock-committor-service/src/committor_processor.rs` | Builds `MagicblockRpcClient` from chain config, optional websocket URI, and optional observed chain-slot atom. |
| `magicblock-committor-service/src/intent_executor/intent_execution_client.rs` | Sends prepared base-layer intent transactions with `ensure_committed()` and records CU metrics through `get_transaction`. |
| `magicblock-committor-service/src/transaction_preparator/delivery_preparator.rs` | Sends delivery/preparation transactions with `ensure_committed()`. |
| `magicblock-committor-service/src/intent_executor/task_info_fetcher.rs` | Fetches committed accounts with `get_multiple_accounts_with_config` and `min_context_slot`. |
| `magicblock-table-mania/src/lookup_table_rc.rs` and `magicblock-table-mania/src/manager.rs` | Create/extend/deactivate/close ALTs, fetch lookup-table metadata/addresses, and choose send confirmation policy. |
| `magicblock-api/src/domain_registry_manager.rs` | Uses `MagicblockRpcClient` for validator domain-registry transaction submission. |
| `magicblock-account-cloner/src/util.rs` | Uses static transaction log/CU extraction helpers for clone diagnostics. |

Main consumers:

- `magicblock-committor-service` for commit/undelegation/action transaction delivery, recovery-related slot reads, task-info fetching, and post-send metrics;
- `magicblock-table-mania` for ALT lifecycle reads and transactions;
- `magicblock-account-cloner` for diagnostic helpers around transaction logs and compute units;
- `magicblock-api` and `magicblock-validator-admin` for operator/admin transaction helpers;
- integration suites `test-integration/test-committor-service` and `test-integration/test-table-mania`.

## Public API shape / Main public types and APIs

### Crate exports

`src/lib.rs` exposes the wrapper and public utilities directly:

- `pub mod utils`;
- `MagicblockRpcClient`;
- `MagicBlockRpcClientError` and `MagicBlockRpcClientResult<T>`;
- `MagicBlockSendTransactionConfig`;
- `MagicBlockSendTransactionOutcome`;
- `SEND_TRANSACTION_ENCODING` and `SEND_TRANSACTION_CONFIG`.

The crate name is `magicblock-rpc-client`; the main type is spelled `MagicblockRpcClient` with a lowercase `b` in `block`. Preserve that spelling in public APIs unless intentionally performing a breaking rename.

### `MagicblockRpcClient`

`MagicblockRpcClient` stores:

- `Arc<RpcClient>` as the underlying Solana RPC client;
- an internal blockhash/slot cache protected by Tokio mutexes;
- optional `Arc<AtomicU64>` `chain_slot` for sharing the highest observed base-layer slot with other services;
- an internal `SignatureConfirmer` for send confirmation.

Constructors:

- `MagicblockRpcClient::new(client)` wraps an RPC client with no chain-slot atom and no websocket URL;
- `new_with_chain_slot(client, chain_slot)` updates the shared atom whenever slots/blockhash context slots are observed;
- `new_with_websocket(client, websocket_url)` enables websocket-first signature confirmation when a URL is provided;
- `new_with_chain_slot_and_websocket(client, chain_slot, websocket_url)` combines both options;
- `impl From<RpcClient>` creates a wrapper by placing the client in an `Arc`.

Common methods:

- `get_latest_blockhash()` fetches/caches `getLatestBlockhash` results and records the response context slot;
- `invalidate_cached_blockhash()` clears only the latest-blockhash cache entry;
- `get_slot()`, `wait_for_next_slot()`, and `wait_for_higher_slot(slot)` read or wait on observed/fetched base-layer slot values;
- `get_account(pubkey)` returns `Ok(None)` for Solana `AccountNotFound` user errors;
- `get_multiple_accounts*` chunks requests, defaulting to 100 pubkeys per RPC call;
- `get_lookup_table_meta` and `get_lookup_table_addresses` deserialize ALT accounts using `AddressLookupTable::deserialize`;
- `request_airdrop`, `get_transaction`, `get_transaction_logs`, and `get_transaction_cus` are convenience pass-through/extraction helpers;
- `get_inner()` returns the underlying `Arc<RpcClient>` for consumers that need a native call.

### Send configuration and outcome

`SEND_TRANSACTION_CONFIG` is the default for actual submission:

- `skip_preflight: true`;
- `encoding: Some(UiTransactionEncoding::Base64)`;
- no preflight commitment, retry override, or min context slot.

`MagicBlockSendTransactionConfig` controls behavior after submission:

- `Send` submits and returns a `MagicBlockSendTransactionOutcome` without status checks;
- `SendAndConfirm` waits for processed status and optionally for the client's configured commitment level;
- `ensure_sent()` returns `Send`;
- `ensure_processed()` waits for processed status, with a 2s blockhash-valid wait hint and a 50s processed timeout;
- `ensure_committed()` waits for processed status and then up to 8s for the client's commitment level;
- `ensures_committed()` reports whether a config includes the commitment-level wait.

`MagicBlockSendTransactionOutcome` carries the signature plus optional processed/confirmed transaction errors. Use `into_result()` when transaction errors should become `MagicBlockRpcClientError::SentTransactionError`; use `into_signature_and_error()` when callers need to log or handle the signature and transaction error separately.

### Errors and retry utilities

`MagicBlockRpcClientError` distinguishes native RPC failures, latest-blockhash/slot failures, ALT deserialize failures, send failures, status timeout/confirmation failures, and submitted transaction errors. `signature()` returns the associated transaction signature for errors that have one. `is_transient()` classifies whether a retry may succeed: ALT deserialize failures and submitted transactions that failed with an on-chain instruction error are deterministic; everything else (transport, RPC availability, unconfirmed status) is transient.

`utils.rs` exposes:

- `send_transaction_with_retries(make_send_fut, mapper, stop_predicate)`, which repeatedly calls an async send operation until success, an unrecoverable mapped error, or a caller-supplied stop condition;
- `SendErrorMapper`, which maps transport/send errors into a caller's execution error and decides retry delay versus break (`decide_flow` takes `&self`, so mappers can carry caller context such as the committor's action-only idempotency guard);
- `TransactionErrorMapper`, which lets domain crates map Solana `TransactionError` values while falling back safely for unknown errors;
- `map_magicblock_client_error` and `try_map_client_error`, used by committor code to preserve domain-specific transaction-error handling;
- `decide_rpc_error_flow` and `decide_rpc_native_flow`, the shared retry policy for known retryable RPC conditions.

Retry policy: IO errors, status-less reqwest errors (connection reset/timeout — the transport-level failures), HTTP 5xx and 429 send errors, node-unhealthy RPC responses, latest-blockhash fetch errors, and signature-status timeout/confirmation failures may retry; unknown transaction errors and remaining client errors break unless mapped by the caller.

## Runtime flows

### Base-layer transaction send and confirmation

```text
caller builds/signs tx
  -> MagicblockRpcClient::send_transaction(tx, config)
  -> RpcClient::send_transaction_with_config(tx, SEND_TRANSACTION_CONFIG)
  -> if config == Send: return signature-only outcome
  -> wait_for_processed_status(signature, tx.recent_blockhash, ...)
  -> optionally wait_for_confirmed_status(signature, client.commitment(), ...)
  -> return outcome or SentTransactionError
```

Important details:

1. `send_transaction` always submits with `skip_preflight: true`; validation must come from callers, transaction construction, and post-send status checks.
2. The processed wait uses `CommitmentConfig::processed()` regardless of the underlying client commitment.
3. The committed wait uses `self.client.commitment()`, so construction sites must choose the desired commitment level on the underlying Solana `RpcClient`.
4. Durable nonce transactions are explicitly unsupported by this helper because confirmation uses the transaction's recent blockhash.
5. If processed status returns a transaction error, `send_transaction` returns `SentTransactionError` immediately and does not continue to the committed wait.

### Signature confirmation with websocket fallback

When a websocket URL is configured:

1. `SignatureConfirmer::wait_for_status` tries `wait_with_websocket_then_poll`.
2. The confirmer obtains or creates a cached `PubsubClient` under a mutex.
3. It subscribes with `signatureSubscribe` and the requested commitment.
4. It races notifications against the fallback delay.
5. A `ProcessedSignature` notification is converted into `TransactionResult<()>`, unsubscribe is called, and the status is returned.
6. Connection/subscription timeout, connection failure, subscription failure, stream end, or fallback delay causes batched polling for the remaining timeout.

Local instrumentation records websocket subscription, notification, and fallback activity for this path.

Do not make websocket confirmation the only path. Polling fallback is required for robustness across RPC providers and transient websocket failures.

### Batched signature-status polling

Without websocket, or after websocket fallback:

1. A waiter is registered in shared `PollState` under the target signature and desired commitment.
2. Completed statuses are looked up from a short-lived cache first.
3. If no polling worker is running, one Tokio task is spawned.
4. The worker sleeps the coalesce delay, snapshots pending signatures, and calls `get_signature_statuses` in chunks of `batch_size`.
5. Fetched statuses are cached and delivered only to waiters whose requested commitment is satisfied by `TransactionStatus::satisfies_commitment`.
6. Waiters whose timeout expires remove themselves from `PollState`.
7. The worker exits after there are no remaining waiters.

Defaults are currently:

- batch size: `256` signatures;
- status cache TTL: `30s`;
- websocket fallback delay: `2s`;
- poll coalesce delay: `25ms`.

Local instrumentation records signature-status batch activity for this path.

### Blockhash and slot caching

`get_latest_blockhash()` caches the latest blockhash for `5s` and keeps recent blockhash metadata for up to the processed-status timeout window. Recent metadata is used to improve status-timeout error messages when the observed slot is still within the blockhash validity window.

Slot reads are cached for `400ms` in `get_cached_slot()`, and both fetched slots and blockhash context slots update the optional shared `chain_slot` atom with `fetch_max`. `wait_for_next_slot()` and `wait_for_higher_slot(slot)` prefer the observed atom when present and otherwise poll the cached slot every `100ms`.

### Batched account and ALT reads

`get_multiple_accounts_with_config` chunks input pubkeys by `max_per_fetch.unwrap_or(100)` and concurrently joins all chunk futures. The default exists because at least some RPC providers reject more than 100 `getMultipleAccounts` inputs. Callers that require `min_context_slot` or specific commitment must pass a full `RpcAccountInfoConfig`.

`get_lookup_table_meta` and `get_lookup_table_addresses` first fetch the raw account through `get_account`; missing accounts return `Ok(None)`, while present accounts must deserialize as Solana ALT state or return `LookupTableDeserialize`.

## Important internals and caveats

### Confirmation state and task spawning

`SignatureConfirmer` has one shared `PollState` per `MagicblockRpcClient` clone set. The state stores waiters by signature, completed status cache entries, and a `worker_running` flag. The polling worker is intentionally demand-driven: it is spawned only when the first waiter is registered and exits when no waiters remain.

Avoid holding the `PollState` mutex across RPC calls. The current implementation snapshots signatures while locked, drops the lock for network I/O, then re-locks to apply statuses. Preserve that shape to avoid blocking new waiters and timeout cleanup behind remote RPC latency.

### Commitment handling

`status_result_for_commitment` only returns a result when the fetched `TransactionStatus` satisfies the requested commitment. This applies to errors as well as successes: a processed transaction error does not satisfy a confirmed waiter until Solana reports a status that satisfies confirmed commitment. Unit coverage exists for this behavior in `transaction_errors_wait_for_requested_commitment`.

### Blockhash validity and retries

`wait_for_processed_status` currently accepts a `_blockhash_valid_timeout` parameter but does not actively wait for blockhash validity through that value. It relies on `SignatureConfirmer` timeout behavior plus cached blockhash metadata for better error text. Do not assume the parameter enforces a separate blockhash-valid wait without changing the implementation and tests.

### Account batching concurrency

`get_multiple_accounts_with_config` launches one future per chunk and joins all chunks concurrently. This reduces wall-clock latency for large fetches but can amplify provider load for very large input lists. If changing chunk size or concurrency, consider both provider limits and settlement/task-info latency.

### Observability contract

This crate owns local confirmation and signature-status instrumentation call sites. Metric naming, labels, and registry details are documented in `.agents/context/crates/magicblock-metrics.md`.

## Important invariants

1. `send_transaction` must keep transaction submission and confirmation semantics explicit: `Send` must not wait for status, and `SendAndConfirm` must not silently skip requested confirmation.
2. The default send path must continue to use the shared `SEND_TRANSACTION_CONFIG` unless callers intentionally opt into a new API; consumers rely on skipped preflight for committor/table-mania delivery behavior.
3. Confirmation waits must respect the requested commitment and must not deliver a cached status that fails `TransactionStatus::satisfies_commitment`.
4. Websocket confirmation must retain batched polling fallback for provider compatibility and transient websocket failures.
5. Polling must remain coalesced and batched; do not replace it with one `getSignatureStatuses` loop per waiter.
6. Do not hold async mutexes across Solana RPC/pubsub network calls.
7. Blockhash and slot caches must be short-lived and monotonic where applicable; `chain_slot` updates must never move the observed slot backward.
8. `get_multiple_accounts*` must preserve input order and output cardinality after chunking.
9. `get_account` must continue to map Solana `AccountNotFound` user errors to `Ok(None)` rather than a hard error.
10. ALT helpers must fail loudly on deserialization errors instead of treating malformed accounts as missing.
11. Retry helpers must preserve caller-owned domain error mapping and must not hide unrecoverable transaction errors behind infinite retries.
12. Public error variants that carry signatures must continue to return them from `MagicBlockRpcClientError::signature()`.

## Common change areas and what to inspect

### Changing send/confirm behavior

Start with:

- `magicblock-rpc-client/src/lib.rs` (`MagicBlockSendTransactionConfig`, `send_transaction`, `wait_for_processed_status`, `wait_for_confirmed_status`);
- `magicblock-rpc-client/src/signature_confirmer.rs` (`wait_for_status`, websocket and polling paths);
- `magicblock-committor-service/src/intent_executor/intent_execution_client.rs` and `transaction_preparator/delivery_preparator.rs`;
- `magicblock-table-mania/src/lookup_table_rc.rs`.

Check that committor and table-mania still know whether a transaction was merely sent, processed, or committed, and that status timeouts remain retryable where intended.

### Changing retry/error mapping

Start with:

- `magicblock-rpc-client/src/utils.rs`;
- `magicblock-committor-service/src/intent_executor/error.rs`;
- `magicblock-committor-service/src/intent_executor/intent_execution_client.rs`;
- `magicblock-committor-service/src/transaction_preparator/delivery_preparator.rs`.

Preserve the distinction between transport/RPC retryability and Solana transaction execution errors. Domain-specific errors should be mapped through `TransactionErrorMapper`; unknown transaction errors should surface with the original signature when available.

### Changing account or lookup-table reads

Start with:

- `get_account`, `get_multiple_accounts*`, `get_lookup_table_meta`, and `get_lookup_table_addresses` in `src/lib.rs`;
- `magicblock-committor-service/src/intent_executor/task_info_fetcher.rs` for `min_context_slot` reads;
- `magicblock-table-mania/src/find_tables.rs`, `manager.rs`, and `lookup_table_rc.rs`.

Preserve provider chunking limits, order/cardinality, requested commitment/config, and `None` semantics for missing accounts.

### Changing slot/blockhash caching

Start with:

- `BLOCKHASH_CACHE_TTL`, `SLOT_CACHE_TTL`, `BlockhashCache`, and `CachedSlot` in `src/lib.rs`;
- committor construction in `magicblock-committor-service/src/committor_processor.rs`;
- `magicblock-table-mania` call sites that fetch latest blockhashes immediately before signing.

Do not lengthen cache lifetimes without considering Solana blockhash expiration and commit-delivery retry behavior.

### Changing metrics

Start with `magicblock-rpc-client/src/signature_confirmer.rs` metric calls and `.agents/context/crates/magicblock-metrics.md` for metric documentation expectations. Keep this guide focused on local instrumentation intent; metric naming, labels, and registry details belong in the metrics guide.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-rpc-client`.
- Relevant integration suites: committor and TableMania suites for confirmation, settlement, or lookup-table behavior; use `.agents/rules/testing-and-validation.md` for exact setup/test commands.
- Performance/security validation intent: report effects on RPC call counts, confirmation latency, waiter batching, websocket fallback rates, and task spawning; preserve explicit send/confirm semantics and transaction-error visibility.


## Adjacent implementation references

- `.agents/context/crates/magicblock-metrics.md` — documentation expectations for RPC-client metrics.
- `.agents/context/crates/magicblock-committor-service.md` — primary send/confirm, task-info, and delivery-preparation consumer.
- `.agents/context/crates/magicblock-table-mania.md` — ALT transaction and finalized remote-read consumer.
- `.agents/context/crates/magicblock-account-cloner.md` — transaction diagnostic helper consumer.
- `.agents/context/crates/magicblock-api.md` — domain-registry and validator wiring consumers.
- `test-integration/test-committor-service/` and `test-integration/test-table-mania/` — integration coverage for settlement and ALT behavior.
