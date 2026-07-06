# `magicblock-metrics`

## Purpose

`magicblock-metrics` is the validator's shared Prometheus metrics crate. It centralizes metric definitions, metric label helper types, metric mutation wrappers, and the small HTTP service that exposes the process-local Prometheus registry.

At a high level it:

- defines the validator-wide Prometheus `Registry` with namespace `mbv`,
- declares gauges, counters, counter vectors, histograms, and histogram vectors used by runtime crates,
- registers all collectors exactly once before the metrics server starts,
- exposes a `/metrics` HTTP endpoint in Prometheus text format,
- provides typed wrapper functions so other crates do not need to construct collectors directly,
- provides reusable label traits/types such as `LabelValue`, `Outcome`, and `AccountFetchOrigin`,
- acts as the observability boundary for RPC, transaction execution, account sync, ledger, committor, RPC client, pubsub/gRPC, table-mania, and system-storage metrics.

This crate is intentionally dependency-light and has no dependency on other workspace crates. Many performance-sensitive crates depend on it, so changes here can affect build topology, hot-path overhead, metric cardinality, and operator visibility across the validator.

## Update requirement

Whenever an agent changes behavior in `magicblock-metrics`, or changes another crate in a way that changes metrics exposed by this crate, this document must be updated in the same change.

Update this file for changes to:

- metric names, help strings, labels, bucket ranges, metric kinds, or namespace behavior,
- wrapper functions in `magicblock-metrics/src/metrics/mod.rs`,
- label enums/traits in `magicblock-metrics/src/metrics/types.rs`,
- metrics service routing, binding, cancellation, or response behavior in `src/service.rs`,
- startup/configuration flow in `magicblock-api` or `magicblock-config` that changes how the service/ticker runs,
- any new consumer crate or major consumer flow that records metrics through this crate,
- validation commands or dashboard/scrape guidance relevant to this crate,
- performance characteristics of metrics used in hot paths.

If a change adds, removes, or renames a metric that operators may scrape or alert on, call that out explicitly in the handoff/PR notes. Metric names are an external observability contract. For metric-related PRs, include at least one practical Prometheus/PromQL query in the PR Details section so reviewers and operators can immediately validate or dashboard the metric change.

For the general documentation-update rule, see `.agents/memory/agent-memory-and-docs.md`.

## Where it sits in the repository

Primary source files:

| Path | Role |
|---|---|
| `magicblock-metrics/src/lib.rs` | Crate exports. Re-exports `metrics`, `try_start_metrics_service`, and `MetricsService`. |
| `magicblock-metrics/src/metrics/mod.rs` | Collector declarations, bucket constants, registry setup, and public mutation/timer wrappers. |
| `magicblock-metrics/src/metrics/types.rs` | Shared metric label helpers (`LabelValue`, `Outcome`, `AccountFetchOrigin`) plus currently-unused account clone/commit shape enums. |
| `magicblock-metrics/src/service.rs` | Hyper/Tokio HTTP server exposing `GET /metrics`, handling cancellation, and encoding registry contents. |
| `magicblock-metrics/README.md` | Prometheus/Grafana setup notes for local scraping and visualization. |
| `magicblock-config/src/config/metrics.rs` | Validator configuration shape for metrics address and system collection frequency. |
| `magicblock-api/src/magic_validator.rs` | Starts the metrics service and system metrics ticker during validator startup. |
| `magicblock-api/src/tickers.rs` | Periodically samples ledger/account storage gauges. |

Main consumers:

- `magicblock-api` starts the metrics service, starts the periodic system metrics ticker, initializes transaction count from restored ledger history, and observes commit nonce wait time through `MagicSysAdapter`.
- `magicblock-aperture` records RPC request counts, RPC handling time, websocket subscription gauges, send-transaction processing time, and skipped-preflight count.
- `magicblock-processor` records slot, transaction count, failed transaction count, and maximum lock-contention queue size.
- `magicblock-ledger` records ledger column counts, storage size, shutdown/compaction/truncation timers, and RPC API column-family metrics.
- `magicblock-chainlink` records account fetch outcomes, chain slot, monitored/evicted accounts, pubsub/gRPC subscription state, undelegation lifecycle counts, and some investigation counters.
- `magicblock-accounts` records scheduled committor-intent count.
- `magicblock-committor-service` records intent backlog, failed intents, busy executors, execution times, CU usage, task/ALT preparation timing, task-info fetcher state, and commit nonce wait time via `magicblock-api`.
- `magicblock-rpc-client` records signature-subscribe and signature-status confirmation counters.
- `magicblock-table-mania` records lookup-table fetch/close investigation counters.

## Public API shape

The crate exports two public areas:

```rust
pub mod metrics;
mod service;

pub use service::{try_start_metrics_service, MetricsService};
```

Consumers normally import either:

```rust
use magicblock_metrics::metrics;
```

or specific collectors/wrappers:

```rust
use magicblock_metrics::metrics::{TRANSACTION_COUNT, RPC_REQUEST_HANDLING_TIME};
```

### Metrics registry and registration

`metrics::REGISTRY` is a lazily-created `prometheus::Registry` with custom namespace `mbv`. A declared metric such as `transaction_count` is exported as `mbv_transaction_count`.

`metrics::register()` registers all collectors into this registry and is guarded by `std::sync::Once`. The function is `pub(crate)`, so production code starts registration through `try_start_metrics_service(...)` rather than calling registration directly.

Important behavior:

- All declared collectors must be registered in `register()` or they will not appear under `/metrics`.
- Registration is one-shot per process. New tests that start the service multiple times should rely on this idempotence instead of attempting to recreate a registry.
- Adding a static collector without adding it to `register()` is a common mistake.
- The custom namespace means dashboards/alerts should use the `mbv_`-prefixed metric names.

### Metrics service

`try_start_metrics_service(addr, cancellation_token)`:

1. calls `metrics::register()`,
2. creates a `MetricsService`,
3. spawns the service on the current Tokio runtime,
4. returns the `MetricsService` handle or an I/O error from construction.

`MetricsService` currently stores the bind address and cancellation token. It does not expose a join handle; `magicblock-api` stores it to keep service ownership tied to validator lifetime.

`start_metrics_server`:

- binds a `tokio::net::TcpListener` to the configured address,
- logs `Metrics server started` when bind succeeds,
- accepts TCP connections until the cancellation token is cancelled,
- spawns one Tokio task per accepted HTTP/1 connection,
- logs accept errors and connection-close debug messages,
- logs `Metrics server shutdown` after cancellation.

`metrics_service_router` behavior:

- `GET /metrics` returns Prometheus text encoding of `metrics::REGISTRY.gather()`.
- All other methods/paths return `404 Not Found` with an empty body.
- It records optional `host` and `user-agent` headers into the tracing span.
- It consumes the entire request body before returning so HTTP keep-alive does not leave unread bytes in the connection buffer.
- If text encoding fails, it logs a warning and returns an empty metrics body rather than panicking.

Pitfalls:

- Do not add heavy per-request work to `/metrics`. Scrapes can happen frequently, and `gather()` already walks every collector.
- Do not add stateful side effects to scrape handling. Prometheus scrapes should observe, not mutate validator state.
- If adding new routes, keep `/metrics` simple and preserve `404` behavior for unknown paths unless there is a clear operator need.
- The service requires an active Tokio runtime because it uses `tokio::spawn` and `TcpListener`.

### Configuration and startup

Metrics are configured through `ValidatorParams.metrics`, backed by `magicblock-config/src/config/metrics.rs`:

```toml
[metrics]
address = "127.0.0.1:9090"
collect-frequency = "30s"
```

Current defaults come from `magicblock-config/src/consts.rs`:

- `DEFAULT_METRICS_ADDR = "0.0.0.0:9000"`,
- `DEFAULT_METRICS_COLLECT_FREQUENCY_SEC = 30`.

`config.example.toml` currently documents a sample address of `127.0.0.1:9090`; check the config defaults before assuming the example value is the runtime default.

Startup flow in `magicblock-api/src/magic_validator.rs`:

1. storage and core services are initialized,
2. `magicblock_metrics::try_start_metrics_service(config.metrics.address.0, token.clone())` starts the HTTP service,
3. `init_system_metrics_ticker(config.metrics.collect_frequency, &ledger, &accountsdb, token.clone())` starts periodic system/storage gauge updates,
4. `TRANSACTION_COUNT.inc_by(ledger.count_transactions()? as u64)` seeds transaction count from persisted ledger history before new execution starts.

Shutdown is cancellation-token based: the system metrics ticker exits when the token is cancelled, and the metrics server stops accepting when the same token is cancelled.

## Metric groups currently defined

### Generic bucket constants

Durations are recorded in seconds because Prometheus histograms use seconds. Shared bucket arrays cover:

- 10-90 microseconds,
- 100-900 microseconds,
- 1-9 milliseconds,
- 10-90 milliseconds,
- 100-900 milliseconds,
- 1-9 seconds.

Several histograms use custom buckets when their expected durations are longer, such as ledger compaction/truncation and committor intent execution.

When adding a histogram, choose buckets around the expected latency distribution. Do not blindly reuse short microsecond/millisecond buckets for operations that normally take seconds or minutes, and do not use only large buckets for hot-path microsecond work.

### Slot

| Wrapper/collector | Meaning |
|---|---|
| `set_slot(slot)` / `SLOT_GAUGE` | Local validator slot. Updated by the processor scheduler. |
| `set_chain_slot(value)` / `CHAIN_SLOT_GAUGE` | Observed base-chain slot. Updated by Chainlink's chain-slot wrapper. |

### Ledger and storage

| Wrapper/collector | Meaning |
|---|---|
| `set_ledger_size(size)` | Ledger storage size in bytes. |
| `set_ledger_block_times_count`, `set_ledger_blockhashes_count`, `set_ledger_slot_signatures_count`, `set_ledger_address_signatures_count`, `set_ledger_transaction_status_count`, `set_ledger_transaction_successful_status_count`, `set_ledger_transaction_failed_status_count`, `set_ledger_transactions_count`, `set_ledger_transaction_memos_count`, `set_ledger_perf_samples_count` | Per-column count gauges updated by ledger metrics collection. |
| `observe_columns_count_duration(f)` | Times ledger column count computation. |
| `start_ledger_truncator_compaction_timer()` | Timer for RocksDB compaction during truncation. |
| `observe_ledger_truncator_delete(f)` | Times deletion of RocksDB slot ranges. |
| `start_ledger_disable_compactions_timer()` | Timer for disabling manual compaction. |
| `start_ledger_shutdown_timer()` | Timer for ledger shutdown. |

System/storage gauge updates are driven from `magicblock-api/src/tickers.rs` at `metrics.collect-frequency`. This means gauges such as ledger size and accounts size are sampled, not updated continuously.

### Accounts and account sync

| Wrapper/collector | Meaning |
|---|---|
| `set_accounts_size(value)` | Persisted account storage size in bytes. |
| `set_accounts_count(value)` | Number of accounts in `AccountsDb`. |
| `set_monitored_accounts_count(count)` | Absolute count of monitored accounts; callers must pass total count, not delta. |
| `inc_evicted_accounts_count()` | Cumulative count of monitored accounts forcefully removed from monitor list/database. |
| `inc_chainlink_bank_precheck_accounts(origin, outcome, reason, count)` / `chainlink_bank_precheck_accounts_total{origin,outcome,reason}` (exported as `mbv_chainlink_bank_precheck_accounts_total`) | Classifies `FetchCloner` account entries before remote provider fetch; call sites should aggregate per label bucket where practical and must not add pubkey labels. |
| `inc_chainlink_subscription_registration_accounts(origin, subscription_reason, outcome)` | `chainlink_subscription_registration_accounts_total` (`mbv_chainlink_subscription_registration_accounts_total`) classifies Chainlink account subscription registration attempts by `{origin,subscription_reason,outcome}`. Origin is `AccountFetchOrigin` label values plus `internal`; subscription reasons are `direct_account`, `delegation_record`, `program_data`, `undelegation_tracking`, `ata_projection`; outcomes are `already_present`, `added_below_capacity`, `evicted_candidate`, `subscribe_error`, `unsubscribe_evicted_error`, `rejected_and_unsubscribed`, `unsubscribe_rejected_error`. |
| `inc_chainlink_subscription_release_accounts(reason, outcome)` | `chainlink_subscription_release_accounts_total` (`mbv_chainlink_subscription_release_accounts_total`) classifies Chainlink account subscription release attempts by `{reason,outcome}` with the same five reason values and outcomes `unsubscribed`, `already_absent`, `unsubscribe_failed`, `retained_intentionally`, `retained_other_reasons`. |
| `inc_chainlink_subscription_cleanup_accounts(cleanup_source, outcome)` | `chainlink_subscription_cleanup_accounts_total` (`mbv_chainlink_subscription_cleanup_accounts_total`) classifies Chainlink account subscription cleanup actions by `{cleanup_source,outcome}`; cleanup sources are `normal_release`, `manual_unsubscribe`, `capacity_eviction`, `rejected_new_subscription`, `delegated_account_silent`, `reconciler`; outcomes are `unsubscribed`, `already_absent`, `unsubscribe_failed`, `removal_update_failed`, `retained_intentionally`. |
| `set_chainlink_unique_pubkeys_estimate(origin, stage, window, estimate)` | `chainlink_unique_pubkeys_estimate{origin,stage,window}` (`mbv_chainlink_unique_pubkeys_estimate`) sets approximate unique Chainlink pubkeys by bounded origin/stage labels and rolling windows `1m`, `5m`, and `1h`. The `stage` argument intentionally accepts `LabelValue` so current-master label enums are reused instead of duplicating stage enums. Example PromQL: `mbv_chainlink_unique_pubkeys_estimate{stage="remote_fetch", window="5m"}`. |
| `inc_account_fetches_success(count)` | Successful network account fetch count. |
| `inc_account_fetches_failed(count)` | Failed network account fetch count. |
| `inc_account_fetches_found(origin, count)` | Network fetches that found accounts, labelled by `AccountFetchOrigin`. |
| `inc_account_fetches_not_found(origin, count)` | Network fetches that did not find accounts, labelled by `AccountFetchOrigin`. |
| `inc_chainlink_clone_accounts_total(origin, remote_result, clone_intent, outcome)` | Chainlink clone lifecycle attempts and outcomes, labelled by bounded enum-like origin, remote-result, clone-intent, and outcome values. |
| `inc_chainlink_clone_materialization_accounts_total(origin, remote_result, outcome)` | Post-clone bank materialization checks, labelled by bounded enum-like origin, remote-result, and materialization-outcome values. |
| `inc_chainlink_empty_placeholder_accounts_total(origin, stage, outcome)` | Empty-placeholder lifecycle events, labelled by bounded enum-like origin, placeholder-stage, and binary outcome values. |
| `inc_undelegation_requested()` | Chainlink observed an undelegation request. |
| `inc_undelegation_completed()` | Chainlink detected undelegation completion. |
| `inc_unstuck_undelegation_count()` | Undelegating account was already undelegated on chain. |
| `inc_chainlink_pending_fetch_accounts(origin, layer, outcome, count)` / `mbv_chainlink_pending_fetch_accounts_total{origin,layer,outcome}` | Account fetch/clone requests by origin, pending-dedup layer, and owner/waiter outcome. |
| `inc_chainlink_pending_fetch_waiters(origin, layer, count)` / `mbv_chainlink_pending_fetch_waiters_total{origin,layer}` | Account fetch/clone requests that joined existing pending work by origin and pending-dedup layer. |
| `inc_chainlink_pending_fetch_waiters_gauge(layer)` / `dec_chainlink_pending_fetch_waiters_gauge(layer)` / `mbv_chainlink_pending_fetch_waiters_gauge{layer}` | Currently active account fetch/clone waiters by pending-dedup layer. |
| `observe_chainlink_pending_fetch_owner_duration_seconds(origin, layer, outcome, seconds)` / `mbv_chainlink_pending_fetch_owner_duration_seconds{origin,layer,outcome}` | Time spent by pending fetch/clone owners by origin, pending-dedup layer, and terminal owner outcome. |

Important caveats:

- `MONITORED_ACCOUNTS_GAUGE` is set to an absolute count. Do not call it with a delta.
- Account fetch found/not-found counters include an `origin` label. Keep origin cardinality low and stable.
- Clone lifecycle counters use only bounded enum-like labels. Do not include pubkeys, signatures, raw errors, endpoints, request parameters, or other unbounded/user-controlled values in these labels.
- `remote_result=failed` is reserved for fetch failures before a clone request is built. Emit it only with `clone_intent=unknown` and `outcome=skipped` unless a later implementation has a concrete clone request to classify.
- The clone lifecycle counters replace the stale clone-cache and pending-clone gauges removed by the eviction-vs-get metrics cleanup; use the counters for clone observability rather than reintroducing those gauges.
- Empty-placeholder stages are bounded enum labels: `converted_to_empty` when Chainlink converts a remote `None` into the zero-lamport/default-owner/empty-data placeholder, `clone_submitted` after a placeholder clone is submitted, `clone_submit_failed` if that clone submission fails, `observed_in_bank_after_ensure` when the post-clone materialization check sees the placeholder in bank, and `still_missing_after_ensure` when the cloner returned success but the placeholder is still not visible. `later_refetched` is reserved for a future sampled/sketch implementation and is not emitted by the current code because retaining per-pubkey state would create unbounded memory/cardinality risk.
- Subscription lifecycle counters are for Chainlink registration, release, and cleanup outcome classification. Call sites must use the provided enum/static labels only; do not add pubkey, signature, raw error, endpoint URL, or other free-form labels.
- Unique Chainlink pubkey estimates use bounded `{origin,stage,window}` labels. The `stage` wrapper parameter accepts any `LabelValue` so existing bounded label enums can be reused; do not add duplicate metric-stage enums in `magicblock-metrics`. Pubkeys and signatures must never be labels; pubkeys are hashed only inside the Chainlink estimator.
- `AccountFetchOrigin::SendTransaction(Signature)` intentionally labels as only `send_transaction`; the signature is available through `signature()` for logging/correlation but must not become a Prometheus label.
- `chainlink_pending_fetch_waiters_gauge` is incremented only for waiter joins, not for owner calls awaiting their own operation. It must be decremented on success, failure, cancellation, timeout, and waiter-drop paths.
- Pending-fetch labels are low-cardinality enum/static labels only: `origin` uses `AccountFetchOrigin`, `layer` uses `fetch_cloner` or `remote_account_provider`, and `outcome` uses exactly `owned`, `joined_existing`, `owner_succeeded`, `owner_failed`, `owner_cancelled`, `resolved_by_subscription_update`, or `rpc_fetch_completed_after_update`.

Useful PromQL examples for the pending-fetch contract use scraped `mbv_` names:

```promql
sum by (origin, layer) (rate(mbv_chainlink_pending_fetch_waiters_total[5m]))
/
sum by (origin, layer) (rate(mbv_chainlink_pending_fetch_accounts_total{outcome="owned"}[5m]))
```

```promql
sum by (origin, layer, outcome) (rate(mbv_chainlink_pending_fetch_accounts_total[5m]))
```

### RPC and aperture

| Wrapper/collector | Meaning |
|---|---|
| `ENSURE_ACCOUNTS_TIME` (`kind` label) | Time spent ensuring account presence. Consumers start timers directly. |
| `TRANSACTION_PROCESSING_TIME` | Total time spent in RPC send-transaction processing. |
| `RPC_REQUEST_HANDLING_TIME` (`name` label) | Time spent handling named RPC requests. |
| `TRANSACTION_SKIP_PREFLIGHT` | Count of transactions submitted with preflight skipped. |
| `RPC_REQUESTS_COUNT` (`name` label) | Count of RPC requests by method name. |
| `RPC_WS_SUBSCRIPTIONS_COUNT` (`name` label) | Active RPC websocket subscriptions by subscription kind. |

Pitfalls:

- RPC method names are labels. Use bounded, canonical method names, not request parameters or user-controlled free-form values.
- Timing wrappers used in hot RPC paths must add minimal overhead. Prefer starting a Prometheus timer once around an existing operation instead of adding multiple nested metrics in tight loops.

### Transaction execution

| Wrapper/collector | Meaning |
|---|---|
| `TRANSACTION_COUNT` | Total executed transactions. Incremented in processor execution; also seeded from ledger on startup. |
| `FAILED_TRANSACTIONS_COUNT` | Total failed transactions. |
| `MAX_LOCK_CONTENTION_QUEUE_SIZE` | Maximum observed queue size for account-lock contention. |
| `set_slot(slot)` | Local slot gauge, updated by scheduler/slot flow. |

Pitfalls:

- Transaction execution and scheduler paths are hot. Do not add expensive label construction, serialization, allocation-heavy formatting, or high-cardinality labels here.
- `TRANSACTION_COUNT` is currently public and sometimes used directly rather than through a wrapper. If changing it, audit all direct imports.

### Committor service and settlement

| Wrapper/collector | Meaning |
|---|---|
| `inc_committor_intents_count()` / `inc_committor_intents_count_by(by)` | Scheduled committor intents. |
| `set_committor_intents_backlog_count(value)` | Number of intents in backlog. |
| `inc_committor_failed_intents_count(intent_kind, error_kind)` | Failed intents labelled by intent kind and error kind. |
| `set_committor_executors_busy_count(value)` | Busy intent executor count. |
| `observe_committor_intent_execution_time_histogram(seconds, kind, outcome)` | Intent execution duration by intent kind and outcome kind. |
| `set_commmittor_intent_cu_usage(value)` | Compute units used for an intent. Note the current function name has three `m`s in `commmittor`; preserve compatibility unless doing a deliberate rename. |
| `observe_committor_intent_task_preparation_time(task_type)` | Timer for task preparation, labelled by task type. |
| `observe_committor_intent_alt_preparation_time()` | Timer for address lookup table preparation. |
| `start_fetch_commit_nonces_wait_timer()` | Timer around waiting for current commit nonce responses from the committor service. |

Pitfalls:

- `LabelValue` is implemented by committor error/output/task types. Make sure new variants return low-cardinality static strings.
- Do not label failed intents with raw error messages, pubkeys, signatures, transaction IDs, or other unbounded values.
- Long-running commit/settlement operations need buckets that make operator alerts meaningful; update bucket ranges if expected durations change materially.

### RPC client, table-mania, task-info, and investigation counters

Current counters include:

- `inc_remote_account_provider_a_count()`,
- `inc_task_info_fetcher_a_count()`,
- `set_task_info_fetcher_retiring_count(count)`,
- `inc_table_mania_a_count()`,
- `inc_table_mania_close_a_count()`,
- `inc_rpc_client_signature_ws_subscribe_count()`,
- `inc_rpc_client_signature_ws_notification_count()`,
- `inc_rpc_client_signature_ws_fallback_count()`,
- `inc_rpc_client_signature_status_batch_count()`,
- `inc_rpc_client_signature_status_batch_signatures_count(count)`.

Some names/help strings are investigation-oriented (`*_a_count`, "Get mupltiple account count"). Treat them as current implementation, but if you formalize or rename them, update dashboards/alerts and this guide in the same change.

### Pubsub clients and gRPC streams

| Wrapper/collector | Meaning |
|---|---|
| `set_connected_pubsub_clients_count(count)` | Total connected pubsub clients. |
| `set_connected_direct_pubsub_clients_count(count)` | Pubsub clients that subscribe immediately when requested. If this goes to zero, account updates may be missed. |
| `set_pubsub_client_uptime(client_id, connected)` | Per-client connection state, `1` for connected and `0` for disconnected. |
| `set_pubsub_client_reconnect_backoff_duration_seconds(client_id, duration_secs)` | Current reconnect backoff. |
| `set_pubsub_client_failed_reconnect_attempts(client_id, attempts)` | Current failed reconnect attempts. |
| `set_pubsub_client_resubscribe_delay(client_id, delay_ms)` | Current resubscription delay in milliseconds. |
| `set_pubsub_client_resubscribed_count(client_id, count)` | Number of subscriptions resubscribed before completion/failure. |
| `set_pubsub_client_connections_count(client_id, count)` | Pooled websocket connection count for a client. |
| `inc_pubsub_unsubscribe_timeout_count(client_id, scope)` | Unsubscribe timeout count. |
| `inc_pubsub_idle_connections_pruned_count(client_id, count)` | Idle pooled connections pruned. |
| `set_grpc_optimized_streams_gauge(client_id, count)` | Optimized gRPC stream count. |
| `set_grpc_unoptimized_streams_gauge(client_id, count)` | Unoptimized gRPC stream count. |
| `set_grpc_total_streams_gauge(client_id, count)` | Total gRPC streams. |

Pitfalls:

- `client_id`, `scope`, and `program` labels must stay bounded. Do not include endpoint URLs with secrets, arbitrary pubkeys unless intentionally bounded, or untrusted free-form strings unless sanitized and cardinality-controlled.
- Pubsub/gRPC metrics are used to detect lost account-update connectivity. Do not silently remove or reset them without replacement guidance.

## Label helper types

### `LabelValue`

`LabelValue` is a small trait used to convert strongly-typed values into stable Prometheus label strings:

```rust
pub trait LabelValue {
    fn value(&self) -> &str;
}
```

It is implemented for:

- `&str`,
- `String`,
- `Result<T, E>` where both sides implement `LabelValue`,
- `AccountFetchOrigin`,
- Chainlink clone lifecycle label enums (`ChainlinkCloneRemoteResult`, `ChainlinkCloneIntent`, `ChainlinkCloneOutcome`, `ChainlinkCloneMaterializationOutcome`, `ChainlinkEmptyPlaceholderStage`),
- Chainlink unique pubkey window labels (`ChainlinkUniquePubkeyWindow`),
- subscription lifecycle label enums (`SubscriptionRegistrationOrigin`, `SubscriptionReasonLabel`, `SubscriptionRegistrationOutcome`, `SubscriptionReleaseOutcome`, `SubscriptionCleanupSource`, `SubscriptionCleanupOutcome`),
- downstream consumer types such as committor execution outputs and errors.

Use `LabelValue` when a metric needs a label derived from an enum-like type. New implementations should return stable, low-cardinality strings. Avoid allocating strings on hot paths where a static `&str` would work.

### `Outcome`

`Outcome` has two variants:

- `Success` -> `success`,
- `Error` -> `error`.

Use `Outcome::from_success(bool)` for binary success/error label values. Do not create separate labels for individual errors unless operators need that distinction and cardinality is controlled.

### `AccountFetchOrigin`

`AccountFetchOrigin` identifies why Chainlink fetched account data:

- `GetMultipleAccounts` -> `get_multiple_accounts`,
- `GetAccount` -> `get_account`,
- `SendTransaction(Signature)` -> `send_transaction`,
- `ProjectAta` -> `project_ata`.

The `SendTransaction` signature is intentionally not part of the label. Use `signature()` for tracing/log correlation only.

### `AccountCommit`

`AccountCommit<'a>` describes account-commit shapes, including commit-only and commit-and-undelegate variants. It is currently defined in `types.rs` for shared metric modelling but is not broadly used by the current wrappers. If you wire it into live metrics, update this guide with its metric names and label behavior.

## Runtime flows

### Validator startup and scrape flow

```text
MagicValidator::try_from_config
  -> try_start_metrics_service(address, cancellation_token)
       -> metrics::register() once
       -> spawn metrics HTTP server
  -> init_system_metrics_ticker(collect_frequency, ledger, accountsdb, token)
  -> seed TRANSACTION_COUNT from ledger count
  -> Prometheus scrapes GET /metrics
       -> REGISTRY.gather()
       -> TextEncoder::encode_to_string(...)
       -> HTTP 200 text body
```

### Periodic system gauge flow

```text
system metrics ticker
  -> sleep(config.metrics.collect_frequency)
  -> ledger.storage_size() -> set_ledger_size
  -> accountsdb.storage_size() -> set_accounts_size
  -> accountsdb.account_count() -> set_accounts_count
  -> repeat until cancellation token is cancelled
```

This flow samples relatively heavyweight storage values off the critical execution path. Do not move these operations into RPC request handling or transaction execution.

### Hot-path instrumentation flow

Most runtime crates instrument an existing operation by:

1. starting a timer or incrementing a counter at the operation boundary,
2. avoiding dynamic label values,
3. recording once on completion/drop,
4. leaving detailed logs/traces to `tracing` rather than Prometheus labels.

Examples:

- RPC `sendTransaction` starts `TRANSACTION_PROCESSING_TIME` once around request processing.
- Processor increments `TRANSACTION_COUNT` once per execution.
- Chainlink increments account-fetch counters by batch counts rather than per-account string labels.
- Committor records intent execution time with typed kind/outcome labels.

## Important invariants

Preserve these invariants when editing this crate:

1. **Metric names are operator-facing API.** Renames/removals require explicit documentation and migration notes.
2. **Every declared collector must be registered exactly once.** Add new collectors to `register()`.
3. **The registry namespace is `mbv`.** Scraped metric names are namespace-prefixed.
4. **Labels must be bounded and low-cardinality.** Never use signatures, pubkeys, account addresses, transaction IDs, raw errors, endpoint URLs with secrets, or user input as labels unless a bounded cardinality design is documented.
5. **Hot-path metrics must be cheap.** Avoid allocations, formatting, locks beyond Prometheus collector internals, and repeated label lookup in tight loops when a batch/outer operation metric is sufficient.
6. **Gauge wrappers must preserve set-vs-delta semantics.** Some wrappers set absolute counts; others increment/decrement. Do not mix these up.
7. **Increment/decrement gauges must be balanced on all control-flow paths.** Do not reintroduce the removed pending-clone gauge; clone observability now comes from the lifecycle counters above.
8. **Histograms must use seconds and meaningful buckets.** Bucket choices should match expected latency ranges.
9. **Scrape handling must not mutate validator state.** `/metrics` observes registry values only.
10. **Metrics service shutdown must remain cancellation-token driven.** Do not introduce shutdown paths that can block validator shutdown indefinitely.
11. **This crate should stay dependency-light.** Do not add dependencies on runtime workspace crates; metrics should be a leaf observability helper that others can depend on.
12. **Do not hide critical failures by removing metrics.** Account-update connectivity, commit backlog/failure, RPC latency, transaction count/failure, and storage-size metrics are operationally important.

## Common change areas and what to inspect

### Adding a new metric

Inspect and update:

1. `magicblock-metrics/src/metrics/mod.rs` for the `lazy_static!` collector declaration.
2. `register()` in the same file.
3. A wrapper function, unless direct collector access is already the pattern for that metric group.
4. The consumer crate where the metric is recorded.
5. This guide's metric group section.
6. Any Prometheus/Grafana docs if the metric is intended for dashboards.

Checklist:

- Is the metric a counter, gauge, histogram, or vector of those?
- Are labels low-cardinality and static/enum-like?
- Are histogram buckets useful for the expected duration?
- Is the metric recorded outside tight loops when possible?
- Is the help string accurate and spelled correctly?
- Does the name follow existing snake_case naming?
- Does the metric need a wrapper to encode semantics?

### Renaming or removing a metric

Treat this as an operator-visible breaking change.

Inspect:

- `magicblock-metrics/src/metrics/mod.rs`,
- consumers listed above,
- `magicblock-metrics/README.md`,
- dashboards/Prometheus configs if present,
- integration tests or scripts that scrape metrics, such as subscription-limit tests.

If a metric is obsolete, prefer a staged approach when possible: keep the old metric while adding the replacement, or document the exact replacement and update all repository references.

The eviction-vs-get metrics plan intentionally removed the stale clone-cache/pending-clone gauges and replaced them with the clone lifecycle, clone materialization, and empty-placeholder counters documented above. Treat that as an operator-visible migration path rather than reintroducing the old gauges.

### Adding or changing labels

Inspect:

- `metrics/types.rs` for reusable label enums,
- downstream `LabelValue` implementations,
- consumer paths to ensure they pass enum/static values rather than dynamic strings.

Pitfalls:

- Prometheus vector label cardinality multiplies memory/time cost.
- A label that includes pubkeys/signatures can create unbounded time-series growth.
- `String` implements `LabelValue`, but that does not make arbitrary strings safe labels.

### Changing metrics service behavior

Inspect:

- `magicblock-metrics/src/service.rs`,
- `magicblock-api/src/magic_validator.rs` startup/shutdown ownership,
- `magicblock-config/src/config/metrics.rs` and config tests,
- `config.example.toml`,
- local Prometheus scrape configuration.

Preserve:

- `GET /metrics` compatibility,
- cancellation-token shutdown,
- body consumption for keep-alive correctness,
- low scrape overhead.

### Changing periodic system metrics

Inspect:

- `magicblock-api/src/tickers.rs`,
- ledger/account storage APIs called by the ticker,
- `MetricsConfig.collect_frequency` defaults and tests.

Do not put storage-size/account-count calls on critical RPC or execution paths. They can involve storage/index work and are intentionally sampled.

### Instrumenting hot paths

Before adding metrics to scheduler, executor, RPC, Chainlink fetch/clone, committor send/confirm, pubsub/gRPC, ledger, or account storage hot paths:

- prefer counters/gauges with no labels or low-cardinality labels,
- avoid string formatting and heap allocation,
- aggregate counts where possible,
- use timers at operation boundaries, not inside per-account/per-instruction loops unless there is a clear need,
- document any unavoidable overhead.

## Tests and validation

- Markdown-only guide changes: run `git diff --check` for this file; no Rust checks are needed.
- Rust changes in this crate: use `.agents/rules/testing-and-validation.md` or `mbv-check`; include focused package checks for `magicblock-metrics`.
- Configuration/startup changes should also include focused checks for `magicblock-config` metrics coverage and `magicblock-api` startup behavior where practical.
- Manual scrape validation is appropriate when changing `service.rs` or startup wiring: start a validator with a known metrics address and verify `/metrics` returns Prometheus text containing `mbv_` metric names while unknown routes return `404`.
- When changing metrics on performance-sensitive paths, include at least a reasoned assessment of label cardinality, allocation/formatting overhead, and scrape/runtime cost.

## Adjacent implementation references

- `magicblock-metrics/README.md` — local Prometheus/Grafana setup.
- `magicblock-config/src/config/metrics.rs` — metrics address and collection-frequency config.
- `magicblock-api/src/magic_validator.rs` — metrics service startup and transaction-count seeding.
- `magicblock-api/src/tickers.rs` — periodic system/storage gauge sampling.
