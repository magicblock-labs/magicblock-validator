# magicblock-aperture

Serves the leader's Solana-style HTTP RPC and WebSocket subscriptions.
Applications use Aperture to submit transactions, read accounts and history,
and follow live updates.

It works with [Chainlink](../chainlink/README.md) to load accounts and
Engine to execute transactions. Verifiers do not run this service.

Embed the service with `Aperture::bind(config, engine, chainlink, legacy, blocktime, cancel)`,
then use its address accessors and consuming `run()`. Binding prepares both listeners
and Geyser subscriptions without spawning delivery tasks. `run()` owns the performance
sampler and Geyser tasks and cancels them on exit.

## Configuration and plugins

Use the [leader configuration](../config.validator.example.toml) for listener addresses,
request limits, and Geyser plugin settings. The WebSocket listener uses the port
immediately after the HTTP port.

Geyser plugins need a shared library compatible with the validator's Rust and
Agave ABI. Notifications expose the data available from Engine, not every field
a full Solana validator would provide. One delivery worker calls every interested
plugin serially, preserving feeder order but not global ordering across Engine's
separate transaction and block streams. Plugin failures are logged independently.
The feeder is bounded; Engine's processed-transaction receiver is not.

Remove `event-processors` from existing configurations,
`MBV_APERTURE__EVENT_PROCESSORS` from the environment, and `--event-processors`
from command lines. These settings are no longer supported. Deployments that used
multiple workers may see lower callback throughput.

## What responses mean

A transaction signature is not proof of base-chain settlement. History comes
from Engine, with a read-only [legacy ledger](../ledger/README.md)
fallback for older records. Aperture preserves Engine's transaction metadata;
it does not repair fees or balances.

See [service interfaces](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/service-interfaces.md)
for detailed client-facing behavior.

[Back to workspace](../README.md)
