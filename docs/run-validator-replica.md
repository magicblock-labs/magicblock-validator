# Run a MagicBlock Validator Replica

Running a replica gives you a locally operated MagicBlock validator that follows
the state of an existing MagicBlock validator. The replica maintains its own
ledger and AccountsDB and exposes its own RPC and WebSocket endpoints.

The replica does not produce an independent sequence of transactions. Instead,
it bootstraps from the primary validator's state and continuously replays the
state transitions published by that validator.

## How replication works

MagicBlock replication follows a primary-replica model and uses NATS JetStream
as the replication broker.

```text
                           NATS JetStream
                    +-------------------------+
                    | Event stream            |
+----------------+  | AccountsDB snapshots    |  +----------------+
| Primary        |->| Consumer state          |->| Your replica   |
| validator      |  +-------------------------+  | validator      |
+----------------+                               +----------------+
                                                         |
                                                  RPC / WebSocket
```

The primary validator publishes transactions, block boundaries, state reset
events, and periodic state checksums to JetStream. It also uploads AccountsDB
snapshots to the NATS object store.

When a new replica starts with empty storage, it:

1. Connects and authenticates to the NATS server.
2. Downloads the latest AccountsDB snapshot, when available.
3. Creates a durable NATS consumer associated with its validator identity.
4. Replays the events published after the snapshot.
5. Continues consuming events and verifies periodic state checksums.

On subsequent starts, the replica uses its persisted local state and durable
consumer position to resume replication.

## Machine requirements

| Resource | Recommended specification |
| --- | --- |
| CPU | 4 cores |
| Memory | 32 GB |
| Storage | 500 GB |
| Network | 10 Gbit/s |
| Operating system | Linux; Ubuntu 24.04 LTS is recommended, but other modern Linux distributions should also work |

Use persistent, high-performance storage for the validator data directory. The
initial snapshot download and continuous event stream also require a stable
network connection.

The appropriate storage capacity depends on the traffic and transaction volume
of the validator being replicated. Treat 500 GB as a baseline for validator
data, provision additional operational headroom, and confirm the final
requirement with MagicBlock for each deployment.

## Prerequisites

Before starting, you need:

- A Linux host that meets the machine requirements above.
- Node.js and npm installed.
- A stable, unique validator identity encoded as a Base58 keypair.
- Outbound connectivity to the NATS server and the configured Solana endpoints.
- The following information from MagicBlock:
  - The NATS server URL.
  - The NATS NKey seed.
  - The authority public key of the validator to replicate.
  - The Solana network of the validator being replicated.

Each replica must use a stable and unique validator identity. The identity is
used to derive the durable NATS consumer name, so it must be preserved across
restarts and must not be shared by multiple replicas.

> [!WARNING]
> The validator keypair and NATS NKey seed are secrets. Do not commit them to a
> repository or include them in logs, support tickets, or screenshots.

## Network access

The host must be able to make outbound connections to:

- The NATS endpoint supplied by MagicBlock.
- The HTTP, WebSocket, and, when applicable, gRPC Solana endpoints in the
  validator configuration.

If clients connect directly to the replica, allow inbound access to the RPC and
WebSocket ports. With the example below, RPC listens on port `8899` and WebSocket
on port `8900`. Port `9000` is used for metrics and should normally remain
restricted to your monitoring network.

## Install the validator

Install the latest validator release from npm:

```bash
npm install -g @magicblock-labs/ephemeral-validator@latest
```

Confirm that the binary is available:

```bash
ephemeral-validator --version
```

Alternatively, download a prebuilt binary from the
[MagicBlock validator releases](https://github.com/magicblock-labs/magicblock-validator/releases)
page on GitHub.

## Prepare the data directory

Create a persistent directory for the replica state. The user running the
validator must have read and write access to it.

```bash
sudo install -d -m 0750 -o "$USER" -g "$USER" /var/lib/magicblock
```

The examples in this guide use `/var/lib/magicblock`. Choose a different path if
required by your environment.

## Create the configuration

Create a file named `replica.toml`:

```toml
lifecycle = "ephemeral"
storage = "/var/lib/magicblock"

# These endpoints must use the same Solana network as the validator being
# replicated. Replace them with your preferred RPC provider endpoints.
remotes = [
  "https://api.devnet.solana.com",
  "wss://api.devnet.solana.com",
]

[validator]
basefee = 0

[validator.replication-mode.replica]
url = "nats://nats.example.com:4222"
secret = "<NATS_NKEY_SEED>"
authority-override = "<PRIMARY_VALIDATOR_AUTHORITY>"

[aperture]
listen = "0.0.0.0:8899"

[metrics]
address = "0.0.0.0:9000"

[accountsdb]
database-size = 4147483648
block-size = "block256"
index-size = 512000000
max-snapshots = 1

[ledger]
block-time = "50ms"
superblock-size = 72000
size = 400000000000

[chainlink]
remove-confined-accounts = false
max-monitored-accounts = 10000

[chain-operation]
claim-fees-frequency = "24h"
```

Replace every placeholder before starting the validator. The example uses
Devnet remotes and starts truncating the ledger as it approaches 400 GB. On a
host with 500 GB of storage, this leaves approximately 100 GB for AccountsDB,
logs, the operating system, and operational headroom. If you are replicating a
Mainnet validator, use Mainnet endpoints instead. You may use public endpoints
or your preferred paid RPC provider, as long as every endpoint belongs to the
same Solana network as the primary validator.

The ledger limit should be evaluated for each deployment. Validators with a
higher transaction volume may require a larger value and additional physical
storage.

### Replication parameters

| Parameter | Description |
| --- | --- |
| `url` | URL of the NATS JetStream server used by the primary validator. MagicBlock provides this value. |
| `secret` | NKey seed used to authenticate with NATS. MagicBlock provides this value through a secure channel. |
| `authority-override` | Public authority of the primary validator being replicated. This is not the local replica identity. |

### Validator identity

Supply the replica's Base58 keypair through the validator environment:

```bash
export MBV_VALIDATOR__KEYPAIR="<LOCAL_VALIDATOR_BASE58_KEYPAIR>"
```

The guide assumes that you already have a Base58 keypair. Keep the same keypair
for the lifetime of the replica.

Restrict access to the configuration file because it contains the NATS seed:

```bash
chmod 0600 replica.toml
```

## Start the replica

Run the validator with the configuration file as its first positional argument:

```bash
RUST_LOG=info ephemeral-validator ./replica.toml --no-tui
```

The `--no-tui` option disables the interactive terminal interface and runs the
validator in headless mode. This is recommended for servers and supervised
processes.

Keep the process attached during the first start so that you can inspect the
bootstrap logs. On a fresh installation, the logs should show that the
validator:

- Connected to NATS.
- Looked for and fetched the AccountsDB snapshot.
- Initialized its replication context and durable consumer.
- Entered replica replication mode.

The initial startup time depends on the snapshot size, disk performance, and
network bandwidth.

## Verify the replica

Request the validator health status:

```bash
curl --silent http://127.0.0.1:8899 \
  --header 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getHealth"}'
```

Query the current slot:

```bash
curl --silent http://127.0.0.1:8899 \
  --header 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getSlot"}'
```

Run the slot request more than once and confirm that the value advances. For a
stronger check, compare selected account state and the current slot with the
primary validator endpoint supplied by MagicBlock.

Monitor the logs for continuity gaps, checksum mismatches, or repeated NATS
reconnections. These errors can indicate that the replica is not following the
primary state correctly even if its RPC endpoint is reachable.

## Run the replica with systemd

After validating the configuration manually, you can use systemd to keep the
replica running. First, find the absolute path of the installed binary:

```bash
command -v ephemeral-validator
```

Create `/etc/magicblock/validator.env` and add the local Base58 keypair:

```bash
MBV_VALIDATOR__KEYPAIR=<LOCAL_VALIDATOR_BASE58_KEYPAIR>
RUST_LOG=info
```

Protect the environment file:

```bash
sudo chmod 0600 /etc/magicblock/validator.env
```

Create `/etc/systemd/system/magicblock-replica.service`:

```ini
[Unit]
Description=MagicBlock Validator Replica
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=<VALIDATOR_USER>
Group=<VALIDATOR_GROUP>
EnvironmentFile=/etc/magicblock/validator.env
ExecStart=/usr/local/bin/ephemeral-validator /etc/magicblock/replica.toml --no-tui
Restart=always
RestartSec=2
LimitNOFILE=1000000

[Install]
WantedBy=multi-user.target
```

Replace `User`, `Group`, and the binary path with values appropriate for your
host. Copy `replica.toml` to `/etc/magicblock/replica.toml`, keep it readable by
the service user, and make sure that user owns or can write to the configured
storage directory.

Reload systemd and start the replica:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now magicblock-replica
```

Follow the logs with:

```bash
sudo journalctl -u magicblock-replica -f
```

## Restart behavior

Do not delete or replace the storage directory during a normal restart. The
validator persists its ledger, AccountsDB, identity information, and replay
position there so that replication can resume without a full bootstrap.

Start the validator again with the same:

- Storage directory.
- Local validator identity.
- NATS credentials.
- Primary validator authority.
- Solana network configuration.

## Troubleshooting

### The validator cannot connect to NATS

- Confirm that the NATS URL is reachable from the host.
- Confirm that the URL scheme and port match the values supplied by MagicBlock.
- Confirm that the NKey seed is the one supplied by MagicBlock.
- Check outbound firewall and DNS rules.

### No snapshot is found

A fresh replica needs a current snapshot to bootstrap efficiently. Confirm with
MagicBlock that the primary validator has uploaded a snapshot and that your NATS
credentials can access the snapshot object store.

### The validator reports a checksum mismatch

A checksum mismatch means that the local AccountsDB differs from the primary at
a verification boundary. Preserve the logs and contact MagicBlock before
removing any local data.

### The validator reports skipped slots or transaction indices

These messages indicate a gap in the replicated event sequence. Check NATS
connectivity and consumer errors, preserve the logs, and contact MagicBlock if
the warning persists.

### The validator identity does not match the ledger

The validator stores its identity alongside the ledger. Reuse the same local
keypair when restarting with an existing storage directory. Contact MagicBlock
before changing the identity of an existing replica.

## Information supplied by MagicBlock

MagicBlock provides the following values for each replica deployment:

| Value | Format |
| --- | --- |
| Network | Devnet or Mainnet |
| Primary validator authority | Solana public key |
| NATS URL | NATS server endpoint |
| NATS NKey seed | Secret authentication seed |

MagicBlock sends the NATS URL and NKey seed through an approved secure channel.
Never publish the NATS seed or the local validator keypair.
