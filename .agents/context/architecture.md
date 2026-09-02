# High-Level Architecture

The workspace separates process roles from reusable runtime construction. For
crate-by-crate ownership, use `.agents/context/crate-map.md`.

```text
                           +--> Chainlink / Aperture / settlement / scheduler
mbv-leader --> Engine<Leader>
      |
      +-------> magicblock-runtime image <-------+
                                                 |
mbv-verifier --> Engine<Follower> + replicator --+

mbv ------> Magic Domain Program over base-layer RPC
mbv-tui --> leader RPC and websocket endpoints
```

## Process roles

`mbv-leader` owns all leader-only service orchestration and orderly shutdown.
Unexpected engine service termination is process-fatal. Magic Domain Program
registration and unregistration are deliberately absent from its lifecycle.

`mbv-verifier` owns only the follower engine and replication client. It must
not start leader RPC, cloning, settlement, scheduling, or admin services. It
does own a process-lifetime metrics endpoint, which remains available while
`ShutdownReason::RestartRequired` closes and reopens the engine after a
snapshot has been staged; other unexpected exits are fatal.

`mbv` and `mbv-tui` are out-of-process operator clients. The former owns
manual Magic Domain Program transactions, while the latter observes compatible
RPC/websocket endpoints.

## Shared execution image

`magicblock-runtime` is the authoritative builder for the Keeper image used by both
engine roles. Native builtins, configured BPF programs, and genesis accounts
must remain shared here to prevent execution drift. Role-specific storage,
replication, and service lifecycle stay in the owning binaries.

## Performance boundaries

Execution, current AccountsDb/ledger persistence, and TCP replication live in
the sibling `../engine` workspace. Leader-side account materialization is owned
by `magicblock-chainlink`; settlement is owned by the committor crates; RPC is
owned by `magicblock-aperture`. Keep operator tooling and process setup off
transaction-critical paths.

The deprecated `magicblock-ledger` remains only for leader historical RPC
fallback during migration.
