# magicblock-aperture

Serves the leader's Solana-style HTTP RPC and WebSocket subscriptions.
Applications use Aperture to submit transactions, read accounts and history,
and follow live updates.

It works with [Chainlink](../magicblock-chainlink/README.md) to load accounts and
Engine to execute transactions. Verifiers do not run this service.

## Configuration and plugins

Use the [leader configuration](../config.validator.example.toml) for listener addresses,
request limits, and Geyser plugin settings. The WebSocket listener uses the port
immediately after the HTTP port.

Geyser plugins need a shared library compatible with the validator's Rust and
Agave ABI. Notifications expose the data available from Engine, not every field
a full Solana validator would provide.

## What responses mean

A transaction signature is not proof of base-chain settlement. History comes
from Engine, with a read-only [legacy ledger](../magicblock-ledger/README.md)
fallback for older records.

See [service interfaces](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/service-interfaces.md)
for detailed client-facing behavior.

[Back to workspace](../README.md)
