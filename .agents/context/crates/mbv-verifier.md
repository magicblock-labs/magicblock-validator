# `mbv-verifier`

`bins/mbv-verifier` runs a bare `Engine<Follower>` and
`replicator::ReplicationClient`.

It uses `VerifierParams`, the shared `magicblock-runtime` image, and an externally
paced engine. It must not start leader-side RPC, Chainlink, settlement,
scheduling, admin, or deprecated-ledger services. It starts the shared metrics
service once for the process lifetime; the endpoint combines MBV collectors
with Engine collectors from the default Prometheus registry.

When replication reports `ShutdownReason::RestartRequired`, terminate the
engine tiers, drop the engine, and reopen from the staged snapshot. Signals
exit cleanly; every other premature service termination is fatal.
The metrics service stays bound across an Engine reopen and is cancelled when
the verifier process exits.

Focused validation:

```bash
cargo check -p mbv-verifier --locked
```
