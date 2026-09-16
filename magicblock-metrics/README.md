# magicblock-metrics

Provides Prometheus metrics for the validator and serves them alongside Engine
metrics at `/metrics`. Leaders and verifiers each have their own endpoint.

## Scraping metrics

Set `[metrics].address` in the process's
[configuration](../magicblock-config/README.md), then add that address to
Prometheus. For example, if the listener is `127.0.0.1:9090`:

```yaml
scrape_configs:
  - job_name: magicblock-validator
    static_configs:
      - targets: ["127.0.0.1:9090"]
```

Use distinct addresses when running multiple processes on one host. A reachable
metrics endpoint does not necessarily mean a verifier has caught up.

## Adding metrics

Keep names, units, and labels useful to operators. Avoid unbounded labels such
as account keys or transaction signatures. Dashboard deployment is separate
from this crate.

[Back to workspace](../README.md)
