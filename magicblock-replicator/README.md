# magicblock-replicator

State replication protocol for streaming validator events via NATS JetStream. Enables primary-standby failover with automatic role transitions.

## Architecture

```
              ┌─────────────┐
              │   Service   │
              └──────┬──────┘
        ┌────────────┴────────────┐
        ▼                         ▼
   ┌─────────┐               ┌─────────┐
   │ Primary │ ← ─ ─ ─ ─ ─ → │ Standby │
   └────┬────┘               └────┬────┘
        │                         │
    ┌───┴───┐                 ┌───┴───┐
    │Publish│                 │Consume│
    │Upload │                 │Apply  │
    │Refresh│                 │Verify │
    └───────┘                 └───────┘
```

The replicator implements a leader-follower pattern where exactly one node acts as **Primary** (publishes events) while others act as **Standby** (consume and replay events). Automatic failover occurs when the primary loses its lock or becomes unresponsive.

## Core Components

### Service

Entry point that manages role transitions. On startup, attempts to acquire the leader lock:
- **Success**: Becomes Primary, starts publishing
- **Failure**: Becomes Standby, starts consuming

The service automatically transitions between roles:
- Primary → Standby: When lock refresh fails or publish errors occur
- Standby → Primary: When leader lock expires and takeover succeeds

### Primary Node

- Holds a distributed leader lock (5-second TTL, refreshed every second)
- Publishes three event types to NATS JetStream:
  - **Transactions**: Individual transaction executions
  - **Blocks**: Slot boundary markers
  - **SuperBlocks**: Periodic state checksums for divergence detection
- Uploads AccountsDb snapshots when new archive files appear in the database directory

### Standby Node

- Consumes events from the stream and replys them locally
- Watches the leader lock for expiration (enables fast takeover)
- Monitors activity timeout (10 seconds of silence triggers takeover attempt)
- Verifies SuperBlock checksums against local AccountsDb state
- Can operate in **ReplicaOnly mode** where takeover is disabled

## NATS JetStream Resources

| Resource | Name | Purpose |
|----------|------|---------|
| Stream | `EVENTS` | Event log (256 GB max, 24h TTL, S2 compression) |
| Object Store | `SNAPSHOTS` | AccountsDb archives (512 GB max) |
| KV Bucket | `PRODUCER` | Leader election lock |

### Event Subjects

- `event.transaction` — Transaction executions
- `event.block` — Block boundaries
- `event.superblock` — State checksums

## Wire Format

Messages are serialized with bincode. Each message includes slot and index for positioning, enabling consumers to track progress and detect gaps.

## Snapshots

Primary nodes watch the AccountsDb directory for new `snapshot-{slot:0>12}.tar.gz` files and upload them to the object store. Snapshots include metadata:
- **Slot**: When the snapshot was taken
- **Sequence**: JetStream sequence number for replay positioning

Standbys can download snapshots for fast recovery instead of full replay.

## Failover Nuances

### Lock Expiration vs Activity Timeout

Two mechanisms trigger standby takeover:

1. **Lock Watcher** (fast): Detects when the leader lock KV entry is deleted/purged
2. **Activity Timeout** (fallback): Triggers after 10 seconds of no messages

The lock watcher provides faster failover, but activity timeout handles cases where the primary crashes without cleanly releasing the lock.

### Pending Message Check

Before attempting takeover on lock expiry, standbys check for pending messages in their consumer. This prevents premature takeover when messages are simply delayed.

### ReplicaOnly Mode

Nodes configured with `can_promote: false` never attempt takeover. They remain in standby mode indefinitely, useful for read-only replicas or debugging.

## Checksum Verification

SuperBlocks carry an AccountsDb checksum. Standbys verify this against their local state after processing. Mismatch indicates state divergence and is logged as an error.

## Threading Model

The service spawns in a dedicated OS thread with a single-threaded Tokio runtime named `replication-service`. This isolation prevents replication work from interfering with the main validator runtime.
