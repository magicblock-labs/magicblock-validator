# magicblock-replicator

State replication protocol for streaming validator events via NATS JetStream. 

## Architecture

```
              ┌─────────────┐
              │   Service   │
              └──────┬──────┘
        ┌────────────┴────────────┐
        ▼                         ▼
   ┌─────────┐               ┌─────────┐
   │ Primary │               │ Replica │
   └────┬────┘               └────┬────┘
        │                         │
    ┌───┴───┐                 ┌───┴───┐
    │Publish│                 │Consume│
    │Upload │                 │Apply  │
    │Refresh│                 │Verify │
    └───────┘                 └───────┘
```

The replicator implements a leader-follower pattern where exactly one node acts as **Primary** (publishes events) while others act as **Replica** (consume and replay events).

## Operator guide

For instructions on installing, configuring, and running a validator replica,
see the [Validator Replica Operator Guide](../docs/run-validator-replica.md).
