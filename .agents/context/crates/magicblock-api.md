# `magicblock-api`

## Purpose

`magicblock-api` is the validator orchestration crate. It turns
`magicblock-config::ValidatorParams` into a running service graph and owns the
`MagicValidator` lifecycle around one `magicblock_engine::Engine`.

The engine owns execution, accountsdb, current transaction/block history,
subscriptions, replay, block pacing, and durable shutdown. MBV keeps validator
policy and services around it: Chainlink, Aperture compatibility, settlement,
scheduled tasks, operator flows, and the temporary deprecated-ledger fallback.

This crate is on startup and shutdown paths. Ordering mistakes can expose RPC
before required state exists, admit work during shutdown, lose settlement work,
or make replicated roles diverge.

## Important paths

| Path | Role |
|---|---|
| `src/magic_validator.rs` | Builds the engine and wires the validator services around it. |
| `src/fund_account.rs` | Builds validator-controlled startup accounts supplied to `KeeperBuilder`. |
| `src/ledger.rs` | Opens the deprecated history ledger and provides its process lock and keypair verification. |
| `src/magic_sys_adapter.rs` | Bridges Magic Program commit-nonce reads to the committor. |
| `src/domain_registry_manager.rs` | Registration, synchronization, and unregistration through the Magic Domain Program. |
| `src/errors.rs` | Orchestration error surface. |

`magicblock-validator/src/main.rs` is the production consumer. It retains the
deprecated ledger lock for process exclusivity and reports that path to the TUI.

## Construction

`MagicValidator::try_from_config` currently performs this sequence:

1. Open the deprecated RocksDB ledger for historical RPC fallback and verify
   the configured signer against the keypair stored beside it.
2. Build `KeeperBuilder` with:
   - the configured signer;
   - engine accountsdb at `<storage>/accountsdb`;
   - engine ledger at `<storage>/ledger`;
   - configured block time, superblock interval, and ledger size;
   - native and Magic Program builtins;
   - configured loader-v4 program ELF bytes;
   - validator-controlled startup accounts and the native mint.
3. Start `Engine::new` with internal pacing for standalone/primary nodes or an
   external pacer for replicas, then retain its `ShutdownManager`. Engine
   construction performs keeper recovery/replay before returning.
4. Spawn the primary replication dispatcher or replica replication client from
   the role-specific authenticated TCP configuration.
5. Build Chainlink with the engine as its read bank and `ChainlinkCloner` target.
6. Build the committor and install `MagicSysAdapter`.
7. Build intent execution, undelegation observation, Aperture, metrics, and task
   scheduling with cloned engine handles.
8. Spawn the dedicated Aperture runtime thread.

The deprecated ledger is not an execution source. Aperture may query it only
after an engine history miss, according to the fallback rules documented in
the Aperture crate guide.

`accountsdb.reset` removes the engine accountsdb directory before construction;
`ledger.reset` removes both the engine ledger directory and the deprecated
history ledger. Engine accountsdb currently has no defragmentation operation,
so `accountsdb.defragment_on_startup` emits a warning instead of silently
claiming that maintenance ran.

## Startup and shutdown

`MagicValidator::start` starts validator-owned services after engine recovery is
complete:

- standalone ephemeral base-layer setup;
- undelegation-request observation;
- periodic fee claims;
- intent execution/recovery;
- the persisted task scheduler.

Engine execution and pacing already run by this point. Do not reintroduce a
parallel scheduler, block ticker, replay path, ledger truncator, or accountsdb
lifecycle in MBV.

`MagicValidator::stop` first stops request ingress, undelegation scheduling,
intent execution, fee claims, and the RPC thread. It then calls
`ShutdownManager::terminate`, which stops the engine in tier order and flushes
its durable state. The deprecated ledger is shut down last.

`prepare_ledger_for_shutdown` now cancels request ingress before the caller
enters the blocking stop path; it no longer controls engine storage.

## Authority and validator-controlled accounts

Outside builtin execution, use `Engine::authority()` for the represented
validator and `Engine::signer()` for local signing. Standalone on-chain fee,
vault, registration, and unregistration flows use the engine signer. Builtins
obtain the execution authority from `nucleus::tls::AUTHORITY`, populated by the
engine executor and simulator.

`fund_account::initial_accounts` supplies:

- the validator identity in `AccountMode::System`;
- MagicContext as Magic Program-owned and delegated;
- the ephemeral vault as Magic Program-owned and ephemeral;
- the native mint as a read-only initialized SPL mint.

These modes are access-control invariants, not incidental startup defaults.

## Replication boundary

The removed MBV replicator used a NATS URL and shared secret. That configuration
is intentionally rejected rather than translated into an invented authorization
policy. Primaries now configure a TCP bind address and explicit follower pubkey
allowlist. Replicas configure the immediate upstream address and authority.

Replica construction sets `Authority::remote` before opening keeper, supplies an
external pacer to `Engine::new`, and passes the matching sender to
`ReplicationClient`. Primary construction keeps internal pacing and starts
`ReplicationDispatcher` with the configured allowlist. An empty allowlist denies
all followers. Replica Aperture is read-only, and Chainlink, intent execution,
scheduled-task execution, and base-layer operator work remain disabled. The
replica's local keypair authenticates its follower connection while
`Engine::authority()` represents the upstream validator.

## Invariants

1. Engine is the only owner of execution, accountsdb, current ledger, replay,
   subscriptions, pacing, and durable shutdown.
2. The deprecated ledger is read-only fallback state and must never feed new
   execution or overwrite engine state.
3. Engine recovery must complete before RPC and validator-owned recovery
   services can accept work.
4. MagicContext and the ephemeral vault must retain their owner and account
   modes.
5. Local signing must use `Engine::signer`; represented authority must use
   `Engine::authority` when replicas are restored.
6. Replicas must use external pacing and authenticate their configured upstream;
   primaries must admit only explicitly allowed follower identities.
7. Shutdown must stop ingress and settlement producers before terminating and
   flushing the engine.
8. Avoid blocking I/O or extra cloning in RPC, Chainlink, or execution paths;
   orchestration work belongs at startup or in bounded background services.

## Validation

Use the narrowest checks that cover an orchestration change:

```bash
cargo check -p magicblock-api
cargo check -p magicblock-validator
cargo clippy -p magicblock-api --all-targets --no-deps -- -D warnings
cargo nextest run -p magicblock-api
```

Run broader workspace checks through `mbv-check`. For changes that enable
replication or alter persistent recovery, add engine replication/recovery tests
and the relevant package tests.

Update this guide whenever constructor ownership, startup/shutdown ordering,
authority handling, storage paths, replication behavior, or service wiring
changes.
