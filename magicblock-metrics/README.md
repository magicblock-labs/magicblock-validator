# `magicblock-metrics`

Validator Prometheus collectors and the HTTP scrape service shared by leader
and verifier processes. Engine collectors remain owned by Engine.

## Serving metrics

`MetricsService::bind` binds the configured socket before background service
startup. `run` serves until its shutdown tier is signalled and reports unexpected
listener termination through the shutdown handle.

`GET /metrics` combines the MBV registry with Prometheus's default registry,
which includes Engine collectors. Other paths return 404. Each host process
owns its endpoint; a verifier keeps the listener alive while reopening Engine
after a staged snapshot.

Configure `[metrics].address` in the role's configuration and use distinct
addresses when running multiple processes on one host. A minimal scrape job is:

```yaml
scrape_configs:
  - job_name: magicblock-validator
    static_configs:
      - targets: ["127.0.0.1:9090"]
```

Use the address actually configured for the process; the leader example uses
9090 and the verifier example uses 9001.

## Instrumentation

The `metrics` module owns validator collectors and context labels. Metric names,
units, and labels are operator-facing interfaces. Prefer bounded categorical
labels and avoid account keys, signatures, or credentials as label values.
Balance in-flight/lifetime counters on failure and shutdown as well as success.

Dashboard and monitoring-stack deployment are separate from this crate.
See [service interfaces][interfaces] for observability boundaries.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[interfaces]: https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/service-interfaces.md
