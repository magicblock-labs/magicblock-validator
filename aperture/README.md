# magicblock-aperture

Serves the leader's Solana-style HTTP RPC and WebSocket subscriptions.
Applications use Aperture to submit transactions, read accounts and history,
and follow live updates.

It works with [ChainSync](../chainsync/README.md) to load accounts and
Engine to execute transactions. Verifiers do not run this service.

Embed the service with `Aperture::bind(config, engine, chainsync, legacy, blocktime, cancel)`,
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

`X-MB-Remote-Account-Claims` counts HTTP account entries fetched by successful
ChainSync calls, including companion accounts and repeated acquisition waves.

See [service interfaces](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/service-interfaces.md)
for detailed client-facing behavior.

## Metrics

Leaders and verifiers expose `GET /metrics` on `[metrics].address`. The standalone
`MetricsService` does not require an RPC service or ChainSync instance. Use distinct
bind addresses when running multiple processes on one host. A reachable endpoint
does not imply that a verifier has caught up.

Collectors live in their owning crate and register in the default Prometheus
registry. Metrics update during normal operations, without a collection ticker or
telemetry-only RPC requests. Names identify the owning component, such as
`aperture_rpc_requests_total` or `committor_inflight_intents`, without a global MBV
prefix. Engine and ChainSync retain their existing series. New collectors appear
when their owner first records an event. Labels describe bounded methods, tasks, or outcomes,
not account addresses or transaction signatures.

For a listener at `127.0.0.1:9090`, configure Prometheus with:

```yaml
scrape_configs:
  - job_name: magicblock-validator
    static_configs:
      - targets: ["127.0.0.1:9090"]
```

[Back to workspace](../README.md)
