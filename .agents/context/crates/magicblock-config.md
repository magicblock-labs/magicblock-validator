# `magicblock-config`

## Purpose

`magicblock-config` owns the validator's typed configuration model and layered configuration loading. It is used by the validator entrypoint and most runtime services to turn defaults, TOML, environment variables, and CLI flags into one `ValidatorParams` value.

High-level responsibilities:

- define strongly typed config sections for validator identity, lifecycle, remotes, RPC/pubsub, metrics, gRPC streams, chainlink, accounts DB, ledger, committor, task scheduler, registration metadata, and preloaded programs;
- parse config from CLI, environment variables, and TOML with deterministic precedence;
- provide small helper types for keypairs/pubkeys, bind addresses, storage paths, and remote endpoints;
- enforce post-load remote defaults so the runtime always has at least one HTTP endpoint and one websocket endpoint;
- keep operator-facing config names in `kebab-case` for TOML and mapped `MBV_...` environment variables.

This crate sits on the startup/configuration path rather than a per-transaction hot path. However, its values control performance-sensitive services such as RPC event processors, account monitoring, gRPC subscription topology, AccountsDb sizing, ledger block timing, metrics collection, and task scheduling. Configuration changes can therefore alter runtime behavior, persistence, operator compatibility, and performance indirectly.

## Update requirement

Update this document in the same change whenever `magicblock-config` behavior or contracts change. This file is useful only if it reflects the current implementation.

Update it for changes to:

- `ValidatorParams`, config section structs, defaults, serde names, or `deny_unknown_fields` behavior;
- precedence, source merging, CLI overlay semantics, environment variable mapping, or TOML file handling;
- public helper types such as `Remote`, `BindAddress`, `StorageDirectory`, `SerdeKeypair`, or `SerdePubkey`;
- operator-facing keys in `config.example.toml` or `magicblock-config/README.md`;
- remote endpoint defaults or HTTP-to-websocket derivation behavior;
- config fields consumed by startup/shutdown, persistence, replication, Chainlink, Aperture, metrics, committor, or task scheduler flows;
- validation commands, integration test coverage, or known pitfalls for config changes.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-config/src/lib.rs` | Crate root, `ValidatorParams`, layered loading in `ValidatorParams::try_new`, remote default helpers, and URL iterators. |
| `magicblock-config/src/config/mod.rs` | Config module declarations and public re-exports for section types. |
| `magicblock-config/src/config/cli.rs` | Clap-facing CLI overlay structs. Only some settings are exposed on CLI. |
| `magicblock-config/src/config/accounts.rs` | AccountsDb storage sizing, block size, snapshot, defragmentation, and reset settings. |
| `magicblock-config/src/config/aperture.rs` | RPC/websocket listen address, event processor count, and Geyser plugin config paths. |
| `magicblock-config/src/config/chain.rs` | Committor compute budget, chain registration metadata, Chainlink cloning/monitoring options, allowed program filters, and Range risk config. |
| `magicblock-config/src/config/grpc.rs` | Global gRPC stream topology limits for remote account providers. |
| `magicblock-config/src/config/ledger.rs` | Ledger block timing, superblock size, reset, keypair verification, size limit, and replay authority override. |
| `magicblock-config/src/config/lifecycle.rs` | `LifecycleMode` and remote-provider requirement helper. |
| `magicblock-config/src/config/metrics.rs` | Metrics bind address and collection frequency. |
| `magicblock-config/src/config/program.rs` | Startup-loadable program ID/path entries. |
| `magicblock-config/src/config/scheduler.rs` | Task scheduler reset, minimum interval, and failed-task retention/cleanup settings. |
| `magicblock-config/src/config/validator.rs` | Base fee, validator identity, replication mode, redacted replication config, and replication authority override helper. |
| `magicblock-config/src/types/` | Serde/parser helper wrappers for keypairs, pubkeys, bind addresses, remotes, and storage directory. |
| `magicblock-config/src/consts.rs` | Default values and remote aliases used by config structs and parsers. |
| `magicblock-config/src/tests.rs` | Unit tests for defaults, precedence, overlays, env mapping, example config coverage, parser helpers, and redaction. |
| `magicblock-config/README.md` | Human-facing crate overview and usage notes. |
| `config.example.toml` | Operator-facing example and coverage target for available options. |
| `test-integration/test-config/` | Integration coverage for config-to-CLI/validator behavior, including allowed program config. |

Main consumers:

- `magicblock-validator/src/main.rs` parses `ValidatorParams`, prints/logs resolved endpoints, and chooses TUI/headless startup.
- `magicblock-api/src/magic_validator.rs` consumes almost every section while constructing ledger, AccountsDb, replication, committor, Chainlink, Aperture, metrics, scheduler, program loading, registration, and recovery flows.
- `magicblock-aperture` consumes `ApertureConfig` for bind addresses, event processors, and Geyser plugin paths.
- `magicblock-chainlink` consumes `ChainLinkConfig`, `GrpcConfig`, `LifecycleMode`, and remote endpoint-derived settings for account sync and stream management.
- `magicblock-accounts-db` consumes `AccountsDbConfig` for persistent account storage sizing, reset, block size, and snapshots.
- `magicblock-task-scheduler` consumes `TaskSchedulerConfig` for SQLite reset, crank timing, and cleanup retention.
- `magicblock-aml` consumes Range risk-related values through `RiskConfig`.
- Integration tests under `test-integration/` parse or mirror config for validator startup scenarios.

## Public API shape / Main public types and APIs

The primary public entrypoint is:

```rust
let params = magicblock_config::ValidatorParams::try_new(std::env::args_os())?;
```

`ValidatorParams` is `Clone + Deserialize + Serialize + Debug + Default` with `#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]`. Its top-level fields are:

- `config: Option<PathBuf>` — optional TOML path parsed positionally by CLI;
- `remotes: Vec<Remote>` — base-chain HTTP/websocket/gRPC endpoints;
- `lifecycle: LifecycleMode` — `ephemeral` by default;
- `storage: StorageDirectory` — root path for persistent validator data;
- `no_tui: bool` — headless mode flag for the validator binary;
- section configs: `metrics`, `grpc`, `validator`, `aperture`, `commit`, `accountsdb`, `ledger`, `chainlink`, `chain_operation`, `task_scheduler`, and `programs`.

Important methods on `ValidatorParams`:

- `try_new(args)` parses CLI with Clap, merges TOML, environment, and CLI providers, extracts the typed struct, then calls `ensure_http()` and `ensure_websocket()`.
- `rpc_url()` returns the first HTTP remote, falling back to `DEFAULT_REMOTE`.
- `websocket_urls()` iterates all websocket remotes.
- `grpc_urls()` iterates all gRPC remotes.
- `Display` serializes the resolved config as pretty TOML when possible, otherwise falls back to debug output.

Public config sections are re-exported from `magicblock_config::config`, including `AccountsDbConfig`, `ApertureConfig`, `ChainLinkConfig`, `ChainOperationConfig`, `CommittorConfig`, `GrpcConfig`, `LedgerConfig`, `LifecycleMode`, `LoadableProgram`, `RiskConfig`, `TaskSchedulerConfig`, and `ValidatorConfig`.

Important helper types:

- `Remote` accepts `http(s)`, `ws(s)`, and `grpc(s)` schemes plus aliases `mainnet`, `devnet`, `testnet`, `localhost`, and `dev`. `grpc`/`grpcs` parse as `Grpc` while rewriting the stored URL scheme to `http`/`https` for URL compatibility.
- `Remote::to_websocket()` derives websocket endpoints only from HTTP remotes, preserving Solana's convention of websocket port = HTTP port + 1 when an explicit port exists.
- `BindAddress` accepts socket addresses and plain port numbers. Plain ports bind to `127.0.0.1:<port>`. `http()` and `websocket()` convert unspecified IPs to localhost for client connection URLs and derive websocket port by saturating `port + 1`.
- `SerdeKeypair` serializes keypairs as base58 strings but displays/debugs only the pubkey. Its clone uses `insecure_clone()` because runtime consumers need signer material.
- `ReplicationConfig` redacts `secret` in `Debug` and `Serialize`; do not replace this with derived implementations.

## Runtime flows

### Layered configuration load

```text
CLI args
  -> CliParams overlay
  -> optional TOML file
  -> MBV_ environment variables
  -> serialized CLI overlay
  -> ValidatorParams extraction
  -> ensure_http + ensure_websocket
  -> runtime startup
```

1. `ValidatorParams::try_new` parses `CliParams` with `CliParams::parse_from(args)`.
2. It starts from an empty `Figment`; serde defaults fill omitted fields during extraction.
3. If `cli.config` is present, `Toml::file(path)` is merged first.
4. Environment variables are merged with prefix `MBV_`, split on `__`, and normalized by replacing `_` with `-` through `Uncased`, so `MBV_LEDGER__BLOCK_TIME` maps to `ledger.block-time`.
5. The serialized CLI overlay is merged last and has highest precedence.
6. The extracted `ValidatorParams` is post-processed to guarantee at least one HTTP remote and one websocket remote.

Precedence is therefore: CLI > environment > TOML > serde/default values. Preserve the optional CLI overlay pattern: a CLI sub-struct must not reset unmentioned TOML/env fields in the same config section.

### Remote endpoint flow

1. `Remote::from_str` recognizes aliases and schemes.
2. Non-standard `grpc` and `grpcs` prefixes are rewritten to `http` and `https` before URL parsing but remain classified as `Remote::Grpc`.
3. `ensure_http()` appends the default devnet HTTP URL when no HTTP remote exists.
4. `ensure_websocket()` appends a websocket derived from the first HTTP remote when no websocket remote exists.
5. `magicblock-api` builds Chainlink `Endpoints` from all remotes, committor chain RPC from `rpc_url()`, and committor websocket from the first `websocket_urls()` result.

Caveat: gRPC remotes do not derive websocket remotes. A config with only gRPC remotes will get a default devnet HTTP and derived websocket remote unless an HTTP/websocket endpoint is also configured.

### Startup consumption flow

```text
magicblock-validator::main
  -> ValidatorParams::try_new
  -> MagicValidator::try_from_config
  -> ledger/accounts/replication/chainlink/aperture/metrics/scheduler/committor startup
```

`magicblock-api/src/magic_validator.rs` is the main consumer. It uses:

- `validator.keypair`, `validator.basefee`, and `validator.replication_mode` for genesis, identity checks, base fees, replication, and mode transitions;
- `ledger` and `storage` for ledger opening, replay, reset, keypair verification, block timing, superblocks, and truncation size;
- `accountsdb` and `storage` for account database open/reset/snapshot behavior;
- `remotes`, `chainlink`, `grpc`, and `lifecycle` for remote account providers, Chainlink configuration, gRPC stream limits, and disabled-chainlink replica mode;
- `aperture` for RPC/websocket server startup and event processing;
- `metrics` for metrics service bind address and collection cadence;
- `commit` for base-layer compute unit price in committor transactions;
- `task_scheduler` for scheduled task service initialization;
- `programs` for startup program loading;
- `chain_operation` only when registration/fee-claim behavior is enabled and lifecycle permits it.

### CLI and file-only fields

Only `CliParams` fields are exposed to CLI. Current CLI coverage includes config path, remotes, lifecycle, storage, no-TUI, metrics bind address, validator base fee/keypair, Aperture listen/event processors, and ledger reset. Many sections are intentionally file/env-only, such as `accountsdb`, `chainlink`, `commit`, `grpc`, `task-scheduler`, `programs`, and most ledger fields. When adding CLI flags, use `Option<T>` plus `skip_serializing_if` so the overlay remains non-destructive.

## Important internals and caveats

### Serde names and environment variables

Most structs use `rename_all = "kebab-case"`; environment variables are upper snake case with `__` for nesting. For example:

- `ledger.block-time` -> `MBV_LEDGER__BLOCK_TIME`
- `task-scheduler.failed-task-cleanup-interval` -> `MBV_TASK_SCHEDULER__FAILED_TASK_CLEANUP_INTERVAL`
- `chainlink.risk.request-timeout` -> `MBV_CHAINLINK__RISK__REQUEST_TIMEOUT`

Do not introduce aliases casually. Operator docs, `config.example.toml`, integration tooling, and deployment configs depend on stable keys.

### Strict unknown-field behavior

Most config structs use `deny_unknown_fields`. This catches typos and stale config but makes renames/removals breaking for operators. If a field is renamed, include migration notes and update tests/example config in the same change.

### Secrets and debug output

`SerdeKeypair` debug/display output is pubkey-only, while serialized config still contains the base58 keypair. `ReplicationConfig` debug/serialize redacts the `secret`. The validator logs `format!("{config:#?}")` on startup, so any new secret-bearing type must implement redaction before being included in debug output.

### Defaults are operational behavior

Defaults are not merely test conveniences. They set devnet remotes, local storage, development validator keypair, base fee, commit compute unit price, account DB size, ledger timing, metrics cadence, Chainlink monitoring capacity, Range risk defaults, task scheduler timings, and gRPC stream limits. Changing defaults can affect local developer flows, integration tests, startup performance, storage usage, and network traffic.

### Config example is tested

`magicblock-config/src/tests.rs::test_example_config_full_coverage` parses the root `config.example.toml` and asserts many values. When adding or changing fields, update the example and this test together where appropriate.

### Lifecycle mode is cross-cutting

`LifecycleMode` is parsed by config but changes how account sync and execution are wired elsewhere. `Offline` is the only mode whose `needs_remote_account_provider()` returns false. Replica replication mode also disables Chainlink in `magicblock-api`; do not assume lifecycle alone fully determines remote-provider usage.

## Important invariants

1. Preserve config precedence: CLI > environment > TOML > defaults.
2. Preserve non-destructive CLI overlay semantics; absent CLI fields must not reset values loaded from lower-precedence sources.
3. Preserve `kebab-case` TOML/serde field names and `MBV_` environment mapping unless an intentional operator-facing breaking change is approved and documented.
4. Keep unknown-field rejection for strict operator feedback unless deliberately changing compatibility behavior.
5. `ValidatorParams::try_new` must return at least one HTTP remote and at least one websocket remote after post-processing.
6. Do not treat gRPC remotes as HTTP/websocket substitutes for committor or JSON-RPC flows; gRPC is for streaming providers.
7. Do not log secret material through `Debug`, `Display`, or startup config logging.
8. Keep `config.example.toml`, `magicblock-config/README.md`, tests, and config structs synchronized.
9. Adding config for a runtime service must include the service consumer update; unused config is misleading operational surface area.
10. Changes to timing, sizing, event-processor, stream-limit, reset, or retention defaults must call out runtime/performance and persistence implications.

## Common change areas and what to inspect

### Adding a new configurable field

Inspect first:

- target section in `magicblock-config/src/config/*.rs`;
- `ValidatorParams` in `magicblock-config/src/lib.rs` if it is a new top-level section;
- service consumer that will read the value;
- `config.example.toml` and `magicblock-config/README.md`;
- `magicblock-config/src/tests.rs` and relevant integration tests.

Checklist:

- choose `kebab-case` TOML name intentionally;
- add a safe/default value or make the field explicitly optional;
- decide whether it is CLI-exposed, env/TOML-only, or TOML-only;
- preserve CLI overlay semantics with `Option<T>` when adding CLI flags;
- add tests for precedence/env/TOML/example coverage when behavior matters.

### Changing remotes or endpoint parsing

Inspect first:

- `magicblock-config/src/types/network.rs`;
- `ValidatorParams::ensure_http`, `ensure_websocket`, `rpc_url`, `websocket_urls`, and `grpc_urls`;
- `magicblock-api/src/magic_validator.rs::init_chainlink` and `init_committor_service`;
- Chainlink endpoint parsing and gRPC stream consumers.

Risks:

- changing default/derived endpoints can silently point validators at a different base chain;
- websocket derivation affects pubsub/account monitoring and committor confirmation behavior;
- `grpc(s)` URL scheme rewriting is relied on by gRPC provider code.

### Changing validator identity or replication config

Inspect first:

- `magicblock-config/src/config/validator.rs`;
- startup identity and replication setup in `magicblock-api/src/magic_validator.rs`;
- ledger keypair verification and replay authority override paths;
- tests for replication secret redaction.

Risks:

- leaking replication secrets or validator keypair material in logs;
- starting with an identity that does not match persisted ledger state;
- accidentally diverging primary and replica startup behavior.

### Changing storage, ledger, or AccountsDb settings

Inspect first:

- `magicblock-config/src/config/accounts.rs` and `ledger.rs`;
- `magicblock-accounts-db/src/lib.rs` and `storage.rs`;
- `magicblock-api/src/ledger.rs` and startup/replay code;
- snapshot/defragment/reset tests.

Risks:

- reset flags wipe persistent state;
- defragmentation and snapshot settings interact with scheduler pauses and startup recovery;
- block time and superblock size affect blockhash validity, snapshots, and metrics.

### Changing Chainlink/gRPC/risk settings

Inspect first:

- `magicblock-config/src/config/chain.rs` and `grpc.rs`;
- `magicblock-api/src/magic_validator.rs::init_chainlink`;
- `magicblock-chainlink/src/remote_account_provider/`;
- `magicblock-aml` Range risk usage;
- allowed-program filtering tests in Chainlink.

Risks:

- subscription limits and resubscription delay affect account sync throughput and provider load;
- allowed-program semantics treat `None` and `Some(vec![])` as unrestricted in current Chainlink code;
- risk config may add external I/O and should remain explicitly disabled by default.

### Changing CLI flags

Inspect first:

- `magicblock-config/src/config/cli.rs`;
- `magicblock-validator/src/main.rs` usage and help output expectations;
- config precedence and overlay tests.

Risks:

- non-optional CLI fields can serialize defaults and overwrite TOML/env values;
- bool flags need careful `skip_serializing_if = "is_false"` handling;
- short flags can conflict with existing options.

## Tests and validation

For documentation-only changes to this guide:

```bash
git diff --check -- .agents/context/crates/magicblock-config.md .agents/context/crate-map.md AGENTS.md
```

Also verify:

- `.agents/context/crates/magicblock-config.md` exists;
- `.agents/context/crate-map.md` points future agents to this guide;
- `AGENTS.md` lists the new crate guide in the crate-specific examples;
- no files under `prompts/**` are staged or committed.

For Rust/source changes in `magicblock-config`, run targeted checks first:

```bash
cargo fmt
cargo clippy -p magicblock-config --all-targets -- -D warnings
cargo nextest run -p magicblock-config
```

For config changes that affect validator startup or operator config, also run the integration config suite when practical:

```bash
cd test-integration
make test-config
```

Broader baseline validation remains the repository standard from `.agents/rules/testing-and-validation.md`:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Performance validation expectations:

- Documentation-only changes have no runtime performance impact.
- Changes to event processor counts, Chainlink monitoring capacity, gRPC stream limits, resubscription delay, ledger block time, AccountsDb sizing, metrics cadence, or task scheduler intervals should report expected runtime impact and include the smallest practical test or measurement for the affected service.
- Changes to reset, replay, identity, remotes, or replication config should include startup/recovery validation, not just crate unit tests.

## Related docs

- `AGENTS.md` — required agent workflow and documentation-memory rules.
- `.agents/context/overview.md` — validator runtime model and important concepts.
- `.agents/context/architecture.md` — startup/service orchestration and configuration ownership.
- `.agents/context/crate-map.md` — crate ownership map and pointer back to this guide.
- `.agents/rules/testing-and-validation.md` — repository validation commands and reporting expectations.
- `.agents/memory/agent-memory-and-docs.md` — rules for keeping agent documentation current.
- `magicblock-config/README.md` — human-facing config crate overview.
- `config.example.toml` — operator-facing example and tested config reference.
- `magicblock-api/src/magic_validator.rs` — primary runtime consumer of `ValidatorParams`.
- `magicblock-validator/src/main.rs` — binary entrypoint and config parsing.
- `test-integration/test-config/` — integration tests for config-driven validator behavior.
