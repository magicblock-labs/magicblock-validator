# magicblock-rpc-client

Read base-chain accounts, send transactions, and track confirmation through a
shared RPC client. For the RPC server that applications connect to, see
[Aperture](../magicblock-aperture/README.md).

## Sending transactions

Choose whether to send only or wait for processing and the requested commitment.
Check the returned outcome as well as the signature: a signature identifies the
transaction, but does not tell you whether it succeeded.

A timeout or lost subscription does not prove that a transaction failed: it may
already have landed. Reconcile its status before retrying work that could have
duplicate effects.

When configured, confirmation uses WebSocket subscriptions with HTTP polling as
a fallback. Without a WebSocket endpoint, it uses HTTP polling directly.
For settlement-specific retry and recovery rules, see the
[committor service](../magicblock-committor-service/README.md).

[Back to workspace](../README.md)
