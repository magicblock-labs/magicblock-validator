# `magicblock-aml`

## Purpose

`magicblock-aml` owns the validator's optional address risk-scoring integration for post-delegation action signers. It wraps the external Range risk API, validates signer risk scores against `magicblock-config`'s `[chainlink.risk]` settings, and persists a small local SQLite cache under the validator ledger path.

High-level responsibilities:

- construct an optional `RiskService` from `RiskConfig`;
- reject enabled risk checking when required configuration is invalid or incomplete;
- query Range's `GET /risk/address?network=solana&address=<pubkey>` endpoint with bearer authentication;
- cache address risk scores in `risk-cache.db` with TTL-based freshness;
- deduplicate concurrent in-flight requests for the same address;
- return `RiskError::HighRiskAddresses` when any checked address score is greater than or equal to the configured threshold.

This crate sits on the `magicblock-chainlink` account synchronization path for accounts with post-delegation actions. It deliberately moves SQLite reads/writes to `spawn_blocking`, but external HTTP calls and cache misses can still delay clone/delegation-action preparation. Keep changes bounded and avoid adding synchronous I/O or unbounded work to Chainlink hot paths.

## Update requirement

Update this document in the same change whenever `magicblock-aml` behavior, public APIs, configuration contract, persistence layout, error semantics, or Chainlink integration changes. This guide is useful only if it reflects the current implementation.

Update it for changes to:

- `RiskService`, `RiskError`, `RiskResult`, cache aliases, or other exported API shape;
- Range endpoint path, query parameters, auth scheme, response parsing, or expected JSON fields;
- `[chainlink.risk]` config keys/defaults in `magicblock-config` or `config.example.toml`;
- cache filename, schema, TTL handling, write timing, or SQLite concurrency behavior;
- in-flight request deduplication, cache-writer lifecycle, or error cleanup;
- which post-delegation action accounts Chainlink risk-checks;
- validation commands, tests, or performance expectations for risk checks.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-aml/Cargo.toml` | Crate manifest. Depends on `magicblock-config`, `reqwest`, `rusqlite`, `tokio`, `serde_json`, and supporting async/error crates. |
| `magicblock-aml/src/lib.rs` | Entire public API and implementation: service construction, cache reads/writes, in-flight deduplication, Range HTTP fetches, and unit tests. |
| `magicblock-config/src/config/chain.rs` | Defines `RiskConfig` under `ChainLinkConfig::risk`; uses kebab-case TOML/env field names. |
| `magicblock-config/src/consts.rs` | Default Range base URL, cache TTL, request timeout, and score threshold. |
| `config.example.toml` | Operator-facing `[chainlink.risk]` documentation and environment variable names. |
| `magicblock-chainlink/src/chainlink/mod.rs` | Creates `RiskService::try_from_config(&chainlink_config.risk, ledger_path)` during Chainlink initialization when a remote account provider exists. |
| `magicblock-chainlink/src/chainlink/fetch_cloner/mod.rs` | Calls `RiskService::check_addresses` for unique signer pubkeys from post-delegation action instructions before action dependencies are fetched/cloned. |
| `magicblock-chainlink/src/chainlink/errors.rs` | Wraps `RiskError` as `ChainlinkError::RangeRisk`. |

Main consumers:

- `magicblock-chainlink` is the only runtime consumer. It stores `Option<Arc<RiskService>>` in `FetchCloner` and skips risk checks when the option is `None`.
- `magicblock-config` owns the config model consumed by this crate; do not duplicate config defaults in AML code.

Important upstream/downstream relationships:

- Upstream input is `RiskConfig` plus the validator ledger path.
- Runtime addresses come from parsed delegation action account metas in Chainlink, currently only metas with `is_signer = true`.
- Downstream effects are Chainlink clone success or `ChainlinkError::RangeRisk(RiskError::...)`; `magicblock-aml` does not mutate account state directly.

## Public API shape / Main public types and APIs

`magicblock-aml` exports everything from `src/lib.rs`.

### Type aliases

- `RiskResult<T> = Result<T, RiskError>` — crate-local result type.
- `PartitionedCache = (Vec<(Option<u64>, String)>, Vec<(Option<u64>, String)>)` — helper shape used when splitting cached and uncached addresses.

### Data structs

- `AddressRiskAssessment { is_high_risk: bool, risk_score: u64 }` — public data shape for risk assessment. It is currently not used by `RiskService::check_addresses`, which returns `Ok(())` or an error instead of per-address assessment data.
- `RiskService` — main service handle. It owns:
  - a `reqwest::Client` with configured timeout;
  - an `Arc<Mutex<rusqlite::Connection>>` for the local SQLite cache;
  - an async mutex-protected in-flight map keyed by address string;
  - an unbounded channel to a background cache writer task;
  - Range `base_url`, API key, cache TTL, and score threshold.

### Errors

`RiskError` is part of the public contract and is converted into `ChainlinkError::RangeRisk`:

- `MissingApiKey` when risk checks are enabled without `api_key`;
- `SqliteInit` for opening or initializing `ledger_path/risk-cache.db`;
- `CacheDirectory` exists in the enum but is not currently emitted by the implementation;
- `Request`, `InvalidJson`, `Sqlite`, `Join` for lower-level failures;
- `RiskScoreNotFound` when the Range response lacks numeric `riskScore`;
- `HighRiskAddresses(Vec<String>)` when checked addresses meet/exceed threshold;
- `InvalidRiskScoreThreshold(u64)` when threshold is greater than `10`;
- `PoisonedLock` for poisoned SQLite mutex;
- `InFlightFetch(String)` when a shared in-flight Range request fails.

### Constructors and methods

- `RiskService::try_from_config(config: &RiskConfig, ledger_path: &Path) -> RiskResult<Option<RiskService>>`
  - returns `Ok(None)` when `config.enabled == false`;
  - requires `api_key` when enabled;
  - rejects `risk_score_threshold > 10`;
  - opens `ledger_path/risk-cache.db` and creates `address_risk_cache` if needed;
  - starts the async cache-writer task;
  - trims trailing `/` from `base_url`.
- `RiskService::check_addresses(addresses: Vec<String>) -> RiskResult<()>`
  - reads fresh cache entries;
  - short-circuits on high-risk cached addresses;
  - fetches missing/stale addresses via Range with in-flight deduplication;
  - asynchronously writes fetched scores to cache;
  - returns `HighRiskAddresses` for fetched scores at or above threshold.

Private helpers such as `fetch_risk_score`, `read_cache`, `write_cache`, `spawn_cache_writer`, and `now_unix_seconds` are implementation details. Do not make callers bypass `RiskService::check_addresses` unless you are intentionally changing the service contract.

## Runtime flows

### Service initialization flow

```text
magicblock-api startup
  -> magicblock-chainlink::InnerChainlink::try_new_from_config
  -> RiskService::try_from_config(chainlink_config.risk, ledger_path)
  -> FetchCloner::new(..., risk_service: Option<Arc<RiskService>>)
```

1. `ChainLinkConfig::risk` is loaded by `magicblock-config` from defaults, TOML, env, and CLI-supported layers.
2. Chainlink creates a `RiskService` only if a remote account provider is active and `RiskConfig::enabled` is true.
3. `RiskService::try_from_config` validates the API key and threshold before opening the SQLite cache.
4. The cache table is created if missing:
   - `address TEXT NOT NULL PRIMARY KEY`
   - `risk_score INTEGER NOT NULL`
   - `fetched_at_unix_s INTEGER NOT NULL`
5. A background Tokio task is spawned to receive fetched scores and write them through `spawn_blocking`.
6. `FetchCloner` stores the service as `Option<Arc<RiskService>>`; `None` means post-delegation action signer checks are disabled.

Caveats:

- The implementation opens `ledger_path/risk-cache.db`; ensure the ledger directory exists before enabling risk checks.
- `CacheDirectory` is currently not used; opening failures surface as `SqliteInit`.
- Enabling risk checks without `api_key` fails Chainlink initialization.

### Post-delegation signer validation flow

```text
delegated account with post-delegation actions
  -> FetchCloner::clone_account_with_post_delegation_action_invariants
  -> ensure_delegation_action_dependencies
  -> validate_post_delegation_action_signers
  -> RiskService::check_addresses(unique signer strings)
```

1. Chainlink parses delegation records and associated post-delegation actions.
2. Before dependency fetch/clone work, `ensure_delegation_action_dependencies` calls `validate_post_delegation_action_signers`.
3. The validator collects all `AccountMeta` pubkeys with `is_signer = true` from each action instruction.
4. Signers are sorted and deduplicated before calling AML.
5. If no `RiskService` is configured, or if no signers exist, validation returns `Ok(())`.
6. `RiskService::check_addresses` must succeed before Chainlink continues to dependency fetching and target cloning.
7. `HighRiskAddresses` or Range/cache errors abort the Chainlink clone path through `ChainlinkError::RangeRisk`.

Caveats:

- AML currently checks signer metas only. Non-signer accounts and program IDs are handled by Chainlink dependency rules, not by `RiskService`.
- A cache miss adds external HTTP latency to the account synchronization path for affected delegation-action clones.

### Address check and cache flow

1. `check_addresses` calls `read_cache(addresses).await`.
2. `read_cache` runs SQLite reads inside `tokio::task::spawn_blocking` and locks the single `rusqlite::Connection` with `std::sync::Mutex`.
3. Entries older than `cache_ttl` are treated as uncached; stale rows are not deleted immediately.
4. Cached scores are checked first. Any cached score `>= risk_score_threshold` returns `HighRiskAddresses` without fetching uncached addresses.
5. Uncached/stale addresses are fetched concurrently with `try_join_all`.
6. `get_or_insert_in_flight` ensures concurrent callers for the same address share one boxed future.
7. If any fetch fails, AML removes the uncached addresses from the in-flight map and returns the failure.
8. Successful fetched scores are sent to the background cache writer and are immediately checked against the threshold.
9. The cache writer upserts all scores and then removes those addresses from the in-flight map.

Caveats:

- Cache writes are asynchronous; tests poll the DB with `assert_eventually_cached` because `check_addresses` can return before the write is visible.
- The cache writer channel is unbounded. Avoid creating call paths that submit unbounded distinct address batches under load.
- A score equal to the threshold is high risk (`>=`, not `>`).

### Range HTTP flow

For each uncached address, `fetch_risk_score`:

1. builds `GET {base_url}/risk/address`;
2. sends query parameters `network=solana` and `address=<address>`;
3. adds bearer auth using `RiskConfig::api_key`;
4. applies `error_for_status()` to reject non-2xx responses;
5. reads the response body as text;
6. parses JSON and extracts a numeric top-level `riskScore` field.

This crate does not currently expose retries, backoff, circuit breaking, batch Range requests, or metrics.

## Important internals and caveats

### SQLite cache and blocking boundaries

The cache is a single `rusqlite::Connection` behind `Arc<Mutex<_>>`. Reads and writes use `spawn_blocking`, which prevents blocking Tokio worker threads but still serializes SQLite access through one connection. Preserve this boundary when changing cache behavior.

Do not perform SQLite operations directly on async runtime tasks. If the cache schema changes, include a migration or compatibility story for existing `risk-cache.db` files.

### In-flight deduplication

The in-flight map stores `Shared<BoxFuture<'static, Result<u64, Arc<str>>>>` by address. This allows multiple concurrent `check_addresses` calls to await the same Range request. Entries are removed after successful cache writes or after failed fetch batches.

Be careful when changing error handling: leaving failed futures in the map would cause repeated callers to observe stale failures; removing entries too early would reintroduce duplicate external calls.

### Threshold and score assumptions

`RiskConfig::risk_score_threshold` must be in the inclusive `0..=10` range. `try_from_config` only rejects values greater than `10`; `0` means every fetched/cached numeric score is high risk.

The Range response parser accepts any JSON document with top-level unsigned integer `riskScore`. It does not validate upper bound on returned scores and does not parse nested assessment details.

### Chainlink ownership boundary

`magicblock-aml` knows nothing about delegation records, cloned accounts, post-delegation dependency ordering, or local account writes. Chainlink decides when to call AML and which addresses to check. Keep account synchronization and delegation-action invariants in Chainlink; keep external risk lookup and cache behavior in AML.

## Important invariants

1. Disabled risk config must return `Ok(None)` and must not require an API key, open SQLite, or spawn background tasks.
2. Enabled risk config must fail fast without an API key and when `risk_score_threshold > 10`.
3. The cache file must remain under the validator ledger path as `risk-cache.db` unless a migration and documentation update are included.
4. SQLite reads/writes must not run directly on async runtime worker threads; keep them behind `spawn_blocking` or an equivalent non-hot-path boundary.
5. Cached scores and fetched scores must use the same high-risk comparison: `score >= risk_score_threshold`.
6. Concurrent requests for the same uncached address must remain deduplicated to avoid avoidable Range API amplification.
7. Failed in-flight fetches must be removed from the in-flight map so later calls can retry.
8. Cache writes may be asynchronous, but fetched scores must still be checked before `check_addresses` returns success.
9. Chainlink must be able to disable AML by passing `None`; AML must not become mandatory for ordinary account cloning.
10. Do not log or expose API keys; keep bearer auth confined to the HTTP request builder.
11. Changes that add latency, retries, batching, or backpressure must explicitly account for Chainlink account-sync hot-path impact.

## Common change areas and what to inspect

### Changing Range request or response handling

Inspect first:

- `magicblock-aml/src/lib.rs::fetch_risk_score`;
- `magicblock-aml/src/lib.rs` mock server tests;
- `config.example.toml` and `magicblock-config/src/consts.rs` if base URL or auth behavior changes.

Risks:

- breaking Range API compatibility;
- changing error classification surfaced through `ChainlinkError::RangeRisk`;
- accidentally logging tokens or full sensitive URLs.

### Changing config defaults or keys

Inspect first:

- `magicblock-config/src/config/chain.rs::RiskConfig`;
- `magicblock-config/src/consts.rs` default constants;
- `magicblock-config/src/tests.rs` config parsing tests;
- `config.example.toml` operator docs;
- `RiskService::try_from_config` validation.

Risks:

- TOML/env field names use kebab-case, for example `risk-score-threshold` and `MBV_CHAINLINK__RISK__RISK_SCORE_THRESHOLD`;
- enabling risk by default would add external network calls to account synchronization and requires an API-key story.

### Changing cache persistence

Inspect first:

- `RiskService::try_from_config` table creation;
- `read_cache`, `write_cache`, and `spawn_cache_writer`;
- unit helpers `load_cached_scores` and `assert_eventually_cached`.

Risks:

- schema changes can break existing cache files;
- removing `spawn_blocking` can block Tokio runtime threads;
- asynchronous writes mean immediate DB assertions can race unless tests poll.

### Changing which addresses are checked

Inspect first:

- `magicblock-chainlink/src/chainlink/fetch_cloner/mod.rs::validate_post_delegation_action_signers`;
- delegation action parsing in `magicblock-chainlink/src/chainlink/fetch_cloner/delegation.rs`;
- post-delegation action clone tests in `magicblock-chainlink/src/chainlink/fetch_cloner/tests.rs`.

Risks:

- checking non-signers or dependencies may materially increase Range requests;
- failing to deduplicate addresses can amplify external calls;
- moving checks later can allow expensive dependency work before rejection.

### Adding retries, batching, metrics, or rate limiting

Inspect first:

- `check_addresses` concurrency and error behavior;
- `get_or_insert_in_flight` shared future lifecycle;
- `magicblock-metrics` for appropriate low-cardinality metric definitions if observability is added;
- Chainlink caller expectations around latency and fail-fast behavior.

Risks:

- retries can delay account clone availability;
- batch APIs may change partial-failure semantics;
- metrics labels must not include addresses or API keys.

## Tests and validation

For documentation-only changes:

```bash
git status --short
ls agents/crates/magicblock-aml.md
grep -n "magicblock-aml" AGENTS.md agents/04_crate-map.md agents/crates/magicblock-aml.md
```

For `magicblock-aml` Rust/source changes, run focused crate checks:

```bash
cargo fmt
cargo clippy -p magicblock-aml --all-targets -- -D warnings
cargo nextest run -p magicblock-aml
```

When changing `RiskConfig` or config defaults, also run:

```bash
cargo nextest run -p magicblock-config
```

When changing Chainlink call sites or which post-delegation action accounts are checked, also run targeted Chainlink tests, for example:

```bash
cargo nextest run -p magicblock-chainlink
```

Broader baseline validation remains the repository standard from `agents/05_testing-and-validation.md` for Rust behavior changes:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance validation expectations:

- Documentation-only changes have no runtime performance impact.
- Any change that increases cache misses, HTTP calls, retries, checked address cardinality, SQLite contention, or Chainlink wait time must report the account-sync hot-path impact and, when practical, validate with representative post-delegation action clone workloads.

## Related docs

- `AGENTS.md` — required agent workflow and documentation-memory rules.
- `agents/00_overview.md` — validator runtime model and important concepts.
- `agents/03_architecture.md` — account synchronization layer and cross-crate boundaries.
- `agents/04_crate-map.md` — workspace crate ownership map and pointer back to this guide.
- `agents/05_testing-and-validation.md` — repository validation commands and reporting expectations.
- `agents/06_agent-memory-and-docs.md` — rules for keeping agent documentation current.
- `agents/crates/magicblock-chainlink.md` — Chainlink account/delegation coordination guide; read it before changing AML call sites.
- `config.example.toml` — operator-facing `[chainlink.risk]` configuration example.
- `magicblock-config/src/config/chain.rs` — `RiskConfig` source of truth.
- `magicblock-chainlink/src/chainlink/fetch_cloner/mod.rs` — runtime caller for post-delegation action signer risk checks.
