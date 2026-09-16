# magicblock-rpc-client

Base-chain RPC helpers for account reads, transaction submission, and signature
confirmation. The application's RPC server is
[Aperture](../magicblock-aperture/README.md), not this crate.

## Sending transactions

Choose whether to send only or wait for processing and the requested commitment.
Inspect the returned outcome, not just its signature, to determine what was
observed.

A timeout or lost subscription does not prove that a transaction failed: it may
already have landed. Reconcile its status before retrying work that could have
duplicate effects.

Confirmation uses WebSocket subscriptions with HTTP polling as a fallback.
The [committor service](../magicblock-committor-service/README.md) owns settlement
retry and recovery decisions.

[Back to workspace](../README.md)
