# Prometheus Grafana Setup

## Installation

### Install Prometheus

- [get it here](https://prometheus.io/docs/prometheus/latest/getting_started/) or just install
  via brew

```sh
brew install prometheus
```

### Install Grafana

- [get it here](https://grafana.com/grafana/download?platform=mac) or just install via brew

```sh
brew install grafana
```

### Optionally Install Node Exporter

This exports metrics of a specific machine, i.e. CPU, memory, disk, etc.

- [get it here](https://prometheus.io/download/#node_exporter)

```sh
brew install node_exporter
```

#### Running Node Exporter


Interact with Node Exporter via `brew service`:

```sh
brew services start node_exporter
brew services info node_exporter
```

Node Exporter listens on `9100` by default.

It exposes multiple metrics, the ones we are interested in are: `node_*`.
Disk: `node_filesystem_avail_bytes`, `node_filesystem_free_bytes`.
MEM:  `node_memory_free_bytes`, `node_memory_total_bytes`.

A dashboard with ID `1860` can be imported to Grafana to visualize these metrics.

### Optionally Install Telegraf

- [get it here](ihttps://www.influxdata.com/time-series-platform/telegraf/)

```sh
brew install telegraf
```

#### Running Telegraf

```sh
brew services start telegraf
```

> The Prometheus Telegraf plugin lets you collect data from HTTP servers exposing metrics in
> Prometheus format.

## Run Prometheus

As service:

```sh
brew services start prometheus
```

With specific config, i.e. monitoring itself:

```sh
prometheus --config.file=prometheus/configs/monitor-self.yml
```

### Sample Config

```yaml
global:
  scrape_interval:     15s

scrape_configs:
  - job_name: 'magicblock-validator'
    scrape_interval: 5s
    static_configs:
      # The URL at which the metrics source is running (our validator)
      - targets: ['localhost:9000']

  - job_name: 'node-exporter'
    scrape_interval: 5s
    static_configs:
      - targets: ['localhost:9100']
```

### See Prometheus Metrics

- http://localhost:9090/metrics
- http://localhost:9090/graph (search for `go_gc_duration_seconds`)

## Connect Grafana to Prometheus

- [follow this guide](https://grafana.com/docs/grafana/latest/getting-started/get-started-grafana-prometheus/)

### Run Grafana

As service:

```sh
brew services start grafana
```

With default config:

```sh
grafana server \
--config /usr/local/etc/grafana/grafana.ini \
--homepath /usr/local/opt/grafana/share/grafana \
--packaging\=brew cfg:default.paths.logs\=/usr/local/var/log/grafana \
  cfg:default.paths.data\=/usr/local/var/lib/grafana \
  cfg:default.paths.plugins\=/usr/local/var/lib/grafana/plugins
```

Access Grafana at http://localhost:3000.

- add prometheus data source (http://localhost:3000/connections/datasources/new)
- connection settings: `http://localhost:9090`
