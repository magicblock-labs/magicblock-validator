# Runtime Architecture

The workspace exposes four independent operator binaries and one shared runtime
crate:

```text
mbv-leader --------------------+
                               +--> magicblock-runtime --> Engine
mbv-verifier ------------------+

mbv ------------> Magic Domain Program over Solana RPC
mbv-tui --------> leader RPC and websocket endpoints
```

## `mbv-leader`

`bins/mbv-leader` owns the `Engine<Leader>` process and the full validator
service graph: Chainlink, Aperture, settlement, scheduling, metrics, and
deprecated-ledger compatibility. It waits on the engine shutdown manager so an
unexpected service exit is process-fatal, and performs orderly service and
engine shutdown.

Domain registration is not part of this lifecycle. Starting or stopping a
leader never sends Magic Domain Program instructions.

## `mbv-verifier`

`bins/mbv-verifier` owns a bare `Engine<Follower>` plus
`ReplicationClient`. It does not start RPC, account cloning, settlement,
scheduling, or admin services. It starts one metrics endpoint for the process
lifetime, combining MBV and Engine collectors. A staged replication snapshot
causes the process to close the engine and reopen it from disk without
rebinding that endpoint; all other unexpected service exits are fatal.

## Shared runtime image

`magicblock-runtime` is the single owner of the Keeper image used by both
roles. It installs the native builtins, configured BPF programs, and genesis
accounts. Keeping this construction shared prevents leader/verifier execution
drift without sharing their process lifecycles.

## Operator clients

`bins/mbv` is a manual CLI. Its `domain register`, `domain sync`, and
`domain unregister` commands use the leader configuration for the base-layer
RPC endpoint and local authority.

`bins/mbv-tui` is an external RPC/websocket monitor. It has no in-process
dependency on leader orchestration and can run on another host.
