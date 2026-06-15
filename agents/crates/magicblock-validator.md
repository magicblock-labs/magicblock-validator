# `magicblock-validator`

## Purpose

`magicblock-validator` is the production binary crate for the MagicBlock validator. It is intentionally thin: it parses `magicblock-config::ValidatorParams`, initializes process-level logging and the main Tokio runtime, constructs `magicblock_api::magic_validator::MagicValidator`, holds the ledger lock for the lifetime of the process, and drives either the headless shutdown-wait loop or the feature-gated embedded TUI.

High-level responsibilities:

- create the process main Tokio runtime with a small worker pool for async I/O and timers;
- load layered CLI/env/TOML/default configuration through `ValidatorParams::try_new`;
- initialize logging through `magicblock-core` or the TUI embedded logger;
- create, start, stop, and report readiness for `MagicValidator`;
- take and retain the ledger write lock so only one validator process uses the same ledger directory;
- expose operator-facing startup information in headless mode;
- wait for `SIGTERM`/`SIGINT` in headless mode and then run the shutdown sequence;
- launch the embedded TUI only when compiled with the `tui` feature and not run with `--no-tui`.

This crate sits on startup and shutdown paths, not on per-transaction execution hot loops. Changes here can still affect operator compatibility, runtime sizing, service ordering, graceful shutdown, ledger durability, and optional TUI behavior.

## Update requirement

Update this guide in the same change whenever `magicblock-validator` behavior or contracts change. In particular, update it for changes to:

- CLI invocation, feature flags, `--no-tui` semantics, or logging behavior;
- main Tokio runtime construction, worker-thread sizing, or runtime/thread names;
- ordering around `ValidatorParams::try_new`, `MagicValidator::try_from_config`, ledger locking, `api.start()`, TUI/headless execution, unregistration, ledger preparation, or `api.stop()`;
- `run_no_tui` startup output, version reporting, endpoint reporting, or shutdown-signal handling;
- `shutdown.rs` signal support or platform-specific graceful shutdown behavior;
- dependencies on `magicblock-api`, `magicblock-config`, `magicblock-core`, `magicblock-version`, or `magicblock-tui-client`;
- validation commands, run commands, packaging, or docs that operators use to start the binary.

Also update this file if another crate changes a contract consumed directly here, such as `MagicValidator` lifecycle methods, `ValidatorParams` fields, ledger lock helpers, or `TuiConfig`.

## Where it sits in the repository

| Path | Role |
|---|---|
| `magicblock-validator/Cargo.toml` | Binary package metadata, `default-run = "magicblock-validator"`, dependencies, and feature flags. `tokio-console` enables Tokio tracing support in `magicblock-core`; `tui` pulls in `magicblock-tui-client`. |
| `magicblock-validator/src/main.rs` | Process entrypoint. Builds the Tokio runtime, parses config, initializes logging, creates/starts/stops `MagicValidator`, holds the ledger lock, and chooses TUI versus headless mode. |
| `magicblock-validator/src/shutdown.rs` | Headless graceful-shutdown waiter. On Unix waits for `SIGTERM` or Ctrl-C; on non-Unix waits for Ctrl-C. |
| `magicblock-api/src/magic_validator.rs` | Downstream lifecycle owner consumed by this binary: `try_from_config`, `start`, `start_unregister_validator_on_chain`, `prepare_ledger_for_shutdown`, `stop`, and `ledger`. |
| `magicblock-api/src/ledger.rs` | Provides public `ledger_lockfile` and `lock_ledger` helpers used here to prevent concurrent ledger use. |
| `magicblock-config/src/lib.rs` and `magicblock-config/src/config/cli.rs` | Define `ValidatorParams`, layered config loading, and CLI flags consumed by the binary, including `--no-tui`. |
| `tools/magicblock-tui-client/` | Optional embedded TUI implementation used only behind the `tui` feature. |
| `README.md`, `docs/architecture.md`, `docs/tui-externalization.md` | Operator and architecture documentation that describe running the validator and the embedded/external TUI split. |

Main consumers:

- End users/operators run this crate as the `magicblock-validator` binary.
- Docker/npm/binary packaging should treat this as the process entrypoint.
- Integration test harnesses and manual workflows may launch this binary indirectly, but runtime behavior is primarily exercised through `magicblock-api` and RPC tests.

Important upstream dependencies:

- `magicblock-api` owns the real validator service graph and lifecycle.
- `magicblock-config` owns config parsing and CLI/env/TOML semantics.
- `magicblock-core` owns logging initialization and optional `tokio-console` feature plumbing.
- `magicblock-version` owns version/git-version display.
- `magicblock-tui-client` owns optional UI rendering and RPC enrichment.

## Public API shape / Main public types and APIs

This is a binary crate, not a library crate. Its public surface is the executable behavior and compile-time features.

### Entrypoint and runtime

- `main()` computes `workers = (num_cpus::get() / 4).max(1)`, builds a multi-thread Tokio runtime with `enable_all()` and thread name `async-runtime`, then `block_on(run())`.
- The worker split is intentional: the main runtime handles general async I/O/timer-bound services while RPC and CPU-bound transaction execution use separate runtimes/threads in downstream crates. Do not casually increase this runtime to consume all CPUs; that can steal capacity from RPC and executor paths.

### Configuration and logging

- `run()` passes `std::env::args_os()` to `ValidatorParams::try_new` and exits with status `1` after printing to stderr if config loading fails.
- Headless builds always call `init_logger()`, which delegates to `magicblock_core::logger::init_with_config` with `LogStyle::from_env()`.
- With the `tui` feature:
  - if `config.no_tui` is true, the normal logger is initialized and the binary runs headless;
  - otherwise `magicblock_tui_client::init_embedded_logger()` supplies a log receiver for the embedded TUI.

### Validator lifecycle calls

The binary calls into `MagicValidator` in this order:

1. `MagicValidator::try_from_config(config).await` to construct the service graph.
2. `ledger::ledger_lockfile(api.ledger().ledger_path())` and `ledger::lock_ledger(...)` to hold the ledger lock for the rest of the process.
3. `api.start().await` to enter primary/replica runtime mode.
4. TUI or headless run loop.
5. `api.start_unregister_validator_on_chain().await` before final shutdown.
6. `api.prepare_ledger_for_shutdown()` to cancel ledger compactions and flush before stop.
7. `api.stop().await` to consume the validator, stop services, join workers, flush AccountsDb/ledger, and shut down the ledger.

Preserve this order unless the `magicblock-api` lifecycle contract changes and both documents are updated together.

### Headless run loop

`run_no_tui(...)` prints startup information including:

- `magicblock_version::Version` and `git_version`;
- local RPC and WebSocket endpoints;
- remote RPC endpoint;
- validator identity;
- ledger location.

It then waits on `Shutdown::wait()`. `print_info` uses plain `println!` only when `RUST_LOG` is unset or exactly `quiet`; otherwise it emits `tracing::info!` so operators can hide startup banners with logging filters such as `RUST_LOG=warn`.

### Feature flags

| Feature | Effect |
|---|---|
| default | Headless binary only. |
| `tui` | Adds `magicblock-tui-client` and enables the embedded TUI path after validator startup. |
| `tokio-console` | Adds `console-subscriber`, enables Tokio tracing, and forwards `magicblock-core/tokio-console`. |

## Runtime flows

### Headless startup and shutdown

```text
main
  -> build main Tokio runtime
  -> run
     -> ValidatorParams::try_new(args)
     -> init_logger
     -> MagicValidator::try_from_config(config)
     -> create and hold ledger write lock
     -> api.start()
     -> run_no_tui(...)
        -> print startup/operator info
        -> wait for SIGTERM/SIGINT (or Ctrl-C on non-Unix)
     -> api.start_unregister_validator_on_chain()
     -> api.prepare_ledger_for_shutdown()
     -> api.stop()
  -> drop runtime
```

Pitfalls:

- The ledger lock guard must remain in scope while the validator runs. Dropping it early would allow another process to open the same ledger directory.
- Shutdown only begins after the headless wait or TUI returns. If a new UI/run loop is added, it must return promptly on operator shutdown.
- `start_unregister_validator_on_chain` is intentionally called before `prepare_ledger_for_shutdown` and `stop`; `MagicValidator::stop` also calls it defensively and no-ops if already started.

### Embedded TUI path

```text
run with --features tui and without --no-tui
  -> init_embedded_logger()
  -> construct/start MagicValidator and lock ledger
  -> build TuiConfig from local endpoints, remote RPC, identity, ledger path, block time, lifecycle, base fee, version
  -> enrich_config_from_rpc(&mut TuiConfig)
  -> run_tui(tui_config, validator_log_rx)
  -> on TUI return/error: continue normal unregister/ledger-prep/stop shutdown
```

Pitfalls:

- The embedded TUI is UI-facing only. Do not move validator service ownership or protocol logic into `tools/magicblock-tui-client`.
- TUI config fields are captured before `MagicValidator::try_from_config(config)` consumes the config. If new display fields are needed, extract them before moving `config`.
- `--no-tui` only matters in builds compiled with the `tui` feature; headless-only builds silence the unused variable and always use normal logging.

### Configuration failure path

If `ValidatorParams::try_new` fails, the binary prints `Failed to read validator config: ...` to stderr and exits with status `1`. If `MagicValidator::try_from_config` fails, it logs the error and exits with status `1`. These are operator-facing process contracts; avoid converting them into panics or silent returns.

## Important internals and caveats

### Keep the binary thin

`magicblock-validator` should remain a process wrapper around `magicblock-api`. Cross-service wiring, account synchronization, scheduler/executor logic, settlement, replication, metrics, and RPC behavior belong in their owning crates. If a change needs to alter runtime behavior after construction, inspect `magicblock-api/src/magic_validator.rs` first.

### Runtime sizing is part of performance behavior

The main runtime intentionally uses about one quarter of available CPUs with a minimum of one worker. The comment in `main.rs` documents that the remaining capacity is reserved for blocking I/O, RPC, and transaction scheduler/executor work. Any change to this split should call out expected impact on startup/shutdown services, RPC latency, and execution throughput.

### Ledger locking is process-level safety

The binary obtains the lock after `MagicValidator` is constructed because it needs the resolved ledger path. It must keep the `RwLockWriteGuard` alive until after the run loop exits. `ledger::lock_ledger` exits the process with an operator-facing message if another validator already holds the lock.

### Shutdown is cooperative with downstream services

`shutdown.rs` only detects process signals. Actual service cancellation, committor shutdown ordering, thread joins, AccountsDb flush, ledger flush, and RocksDB shutdown are owned by `MagicValidator::stop`. Do not duplicate that logic in the binary.

### Operator-facing output is compatibility-sensitive

Startup output in `run_no_tui` is useful for local development, automation, and debugging. If changing labels, hiding fields, or routing output differently, update operator docs and consider tests or manual validation of both `RUST_LOG` modes.

## Important invariants

1. The binary must not own protocol, account, execution, RPC, settlement, or persistence logic beyond process lifecycle calls into `magicblock-api`.
2. `ValidatorParams` must be parsed before any config-dependent logging, TUI choice, endpoint reporting, or validator construction.
3. The ledger write lock guard must stay alive while `MagicValidator` is running.
4. `api.start()` must complete before the binary reports readiness or launches the embedded TUI.
5. Shutdown must call `start_unregister_validator_on_chain`, `prepare_ledger_for_shutdown`, and `stop` in the established order unless the `magicblock-api` lifecycle changes.
6. The main Tokio runtime must leave CPU capacity for RPC and transaction execution domains; avoid moving blocking or CPU-bound work onto it.
7. The embedded TUI must remain feature-gated and must not be required for headless operation.
8. `SIGTERM` and Ctrl-C should initiate graceful shutdown on Unix; Ctrl-C should remain supported on non-Unix.
9. Version, endpoint, identity, and ledger-path reporting should remain accurate and derived from the resolved config/runtime state.
10. Error paths during config loading or validator construction should fail fast and visibly for operators.

## Common change areas and what to inspect

### Changing startup or shutdown order

Start with:

- `magicblock-validator/src/main.rs` (`run`, `run_no_tui`);
- `magicblock-api/src/magic_validator.rs` (`try_from_config`, `start`, `start_unregister_validator_on_chain`, `prepare_ledger_for_shutdown`, `stop`);
- `agents/crates/magicblock-api.md` startup/shutdown sections.

Check that ledger locking, readiness reporting, unregister behavior, compaction cancellation, service cancellation, and durable flushes still happen in a safe order.

### Changing CLI/config behavior

Start with:

- `magicblock-config/src/lib.rs` (`ValidatorParams::try_new`);
- `magicblock-config/src/config/cli.rs` (`CliParams`, `CliValidatorConfig`, `CliApertureConfig`, `CliLedgerConfig`);
- `agents/crates/magicblock-config.md`.

Do not add ad-hoc CLI parsing in the binary; keep config layering in `magicblock-config`.

### Changing logging or startup output

Start with:

- `magicblock-validator/src/main.rs` (`init_logger`, `print_info`, `run_no_tui`);
- `magicblock-core/src/logger/`;
- `tools/magicblock-tui-client/` if embedded TUI logs are affected.

Validate both unset/`quiet` `RUST_LOG` behavior and filtered tracing behavior.

### Changing TUI integration

Start with:

- `magicblock-validator/Cargo.toml` feature flags;
- `magicblock-validator/src/main.rs` `#[cfg(feature = "tui")]` blocks;
- `tools/magicblock-tui-client/README.md` and `docs/tui-externalization.md`.

Keep the default binary headless and preserve `--no-tui` behavior for feature-enabled builds.

### Changing runtime sizing or async behavior

Start with:

- `main()` runtime builder;
- `magicblock-api/src/magic_validator.rs` for downstream RPC/execution runtime/thread creation;
- `agents/03_architecture.md` and `agents/crates/magicblock-api.md` for execution-domain boundaries.

Avoid blocking calls in the main async runtime unless they are already isolated in downstream service code.

## Tests and validation

For documentation-only changes touching this guide:

```bash
git diff --check -- agents/crates/magicblock-validator.md agents/04_crate-map.md AGENTS.md
```

For code changes in this crate, run at minimum:

```bash
cargo fmt
cargo check -p magicblock-validator --no-default-features
cargo check -p magicblock-validator --features tui
cargo nextest run -p magicblock-validator
```

If logging or `tokio-console` behavior changes, also check:

```bash
cargo check -p magicblock-validator --features tokio-console
```

For broader validation before handoff, follow `agents/05_testing-and-validation.md`:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

For lifecycle or startup/shutdown changes, perform a manual smoke run with a disposable storage directory or the relevant integration harness. At minimum verify the process starts, prints expected endpoints/identity/ledger path, accepts Ctrl-C/SIGTERM, and exits after flushing without leaving a second process able to use the same ledger concurrently.

Performance-sensitive risk to report: runtime sizing, blocking work in `run`, and lifecycle ordering can indirectly affect RPC/execution throughput or shutdown latency. If those areas change, state whether any performance or smoke validation was run.

## Related docs

- `agents/00_overview.md` — validator runtime model and non-negotiable agent rules.
- `agents/03_architecture.md` — process/service orchestration and startup/shutdown boundaries.
- `agents/04_crate-map.md` — repository crate ownership map.
- `agents/05_testing-and-validation.md` — repository validation workflow.
- `agents/crates/magicblock-api.md` — downstream `MagicValidator` lifecycle and service graph.
- `agents/crates/magicblock-config.md` — config layering and CLI/env/TOML behavior.
- `agents/crates/magicblock-core.md` — logging and shared runtime infrastructure.
- `README.md` — operator-facing build/run overview.
- `docs/architecture.md` — high-level architecture, including this binary as the entrypoint.
- `docs/tui-externalization.md` and `tools/magicblock-tui-client/README.md` — embedded/external TUI behavior.
