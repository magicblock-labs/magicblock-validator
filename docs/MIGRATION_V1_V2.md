# V1 → V2 migration

- [Prepare](#prepare)
- [Configuration changes](#configuration-changes)
- [Cutover](#cutover)
- [Validation](#validation)
- [Replication](#replication)
- [Retire legacy history](#retire-legacy-history)
- [References](#references)

## Prepare

Stage **v2.0.0** binaries (`magicblock-validator`, `magicblock-verifier`, and
`magicblock`) and [adb-migrator 1.4.0](https://github.com/magicblock-labs/adb-migrator/blob/1.4.0/README.md).
Before stopping V1, save a few account samples and transaction signatures for
the post-migration checks.

Stage `hydra-cranker` as a separate systemd service/container on the ER host,
with a keypair funded on the ER. For Linux x64:

```sh
npm install -g @magicblock-labs/hydra-cranker-linux-x64@0.2.1
```

Use the matching platform package on other hosts. Ephemeral Hydra is already
deployed on devnet and mainnet; no on-chain deployment is needed. See
[Hydra operation](https://github.com/magicblock-labs/magicblock-validator/issues/1815).

For Yellowstone, use the [MBV 2.0 branch](https://github.com/magicblock-labs/yellowstone-grpc/tree/mbv-2.0.0)
and its [plugin build/config guide](https://github.com/magicblock-labs/yellowstone-grpc/blob/mbv-2.0.0/yellowstone-grpc-geyser/README.md).
Set the plugin JSON's `libpath` to the rebuilt `.so` and list that JSON under
`aperture.geyser-plugins`. The [sample JSON](https://github.com/magicblock-labs/yellowstone-grpc/blob/mbv-2.0.0/yellowstone-grpc-geyser/config.json)
covers listeners, TLS, and filters.

### Storage

Recommended on both leader and verifiers: **AccountsDB on a separate CoW-capable
filesystem/partition from the ledger**—Btrfs, XFS with reflink enabled, or equivalent.
Engine clones the active AccountsDB tree for snapshots; reflinks avoid copying
unchanged extents.

Example layout:

```text
/home/magicblock/ledger/                  # V1 source
├── accountsdb/main/
├── committor_service.sqlite
├── task_scheduler.sqlite
└── rocksdb/

/srv/magicblock-v2/ledger/                # ledger mount
└── rocksdb -> /home/magicblock/ledger/rocksdb

/srv/magicblock-v2/accountsdb/            # separate CoW mount
└── migrated/                            # migrator creates this directory
```

Allow space for migration staging and snapshot extents on the AccountsDB mount,
and compressed snapshot archives and ledger growth on the ledger mount. Verify
reflink support with `cp --reflink=always` on a nonempty temporary file.

**Size ledger retention around the existing disk usage.** Engine's `size-limit`
measures total used bytes on the ledger filesystem, including legacy RocksDB and
any other files on that filesystem. A symlink to another filesystem doesn't count
the target's usage against this limit. See [retention accounting](https://github.com/magicblock-labs/magicblock-engine/blob/0.23.1/ledger/README.md#retention).

If the legacy ledger already occupies 60–70% of the filesystem, provision enough
capacity for several days of coexistence. Set `size-limit` above the existing
usage plus the desired V2 history/snapshot budget, while leaving free-space
headroom below filesystem capacity. Expand the disk/filesystem if that won't fit;
expansion alone doesn't raise `size-limit`. If the threshold is already exceeded,
Engine prunes its own sealed superblocks—it doesn't remove the legacy ledger.

For example, on a 1 TiB filesystem with 700 GiB already used, a 900 GiB limit
allows roughly 200 GiB for V2 growth and leaves 124 GiB below filesystem capacity.

### Programs and healthchecks

Download [v42 calculator](https://github.com/magicblock-labs/magicblock-engine/blob/0.23.2/programs/v42-calculator-program/README.md)
from the same release as the validator for the operator healthcheck:

```sh
gh release download v2.0.0 --repo magicblock-labs/magicblock-validator \
  --pattern v42_calculator_program.so --dir /opt/magicblock/programs
```

Use matching program IDs and ELF files on the leader and verifiers. Add v42 to
both configs:

```toml
[[programs]]
id = "V42CaLcu1atormagicb1ock11111111111111111111"
path = "/opt/magicblock/programs/v42_calculator_program.so"
```

The healthcheck account is created automatically. Existing persisted program
images are preserved; changing the configured ELF doesn't replace them.
See [runtime program persistence](../runtime/README.md#integration).

## Configuration changes

Create configs from the V2 [leader](../config.validator.example.toml) and
[verifier](../config.verifier.example.toml) examples. Apply the following changes
to TOML and service environment files; V2 rejects unknown settings.

### Move or replace

| V1 setting | V2 setting | Action |
| --- | --- | --- |
| `validator.keypair` | `engine.authority.local` | Copy the existing leader's base58-encoded keypair, not its public key or a keypair-file path. |
| `storage` | `engine.ledger.directory` | Point to the new V2 ledger root containing the legacy `rocksdb` symlink. |
| `storage` → `accountsdb/main` | `engine.accountsdb.directory` | Set to the migrator's `DESTINATION`, on the separate AccountsDB mount. Do not point at V1's `accountsdb/main`. |
| `ledger.block-time` | `engine.blockstore.blocktime` | Preserve the deployed cadence; use the same milliseconds for `--slot-duration-ms`. |
| `ledger.superblock-size` | `engine.blockstore.superblock` | Copy the slot count, or choose the new sealing interval deliberately. |
| `ledger.size` | `engine.ledger.size-limit` | Both are bytes; size the new limit for total filesystem usage, including legacy history. See [storage](#storage). |
| `validator.replication-mode` | `engine.replication` | Replace NATS with the leader TCP listener/allowlist and separate verifier configs; see [replication](#replication). NATS URL/secret values are not reused. |
| `validator.replication-mode.replica.authority-override` | Verifier `engine.replication.upstream-authority` | Use the leader's public key. The verifier's own keypair belongs in `engine.authority.local`. |
| `chainlink.risk.base-url`, `api-key`, `cache-ttl`, `risk-score-threshold` | Separate risk-server configuration | Move provider credentials, cache, and threshold to the risk server. Set `chainlink.risk.risk-server-url` to that server—not the old Range API URL. |

After moving values out of `[validator]`, remove that section. `[ledger]` remains
for `block-cache-size`, `verify-keypair`, and `reset`; remove its moved fields.
Keep `ledger.reset = false` for the migration.

### Remove

| V1 setting | V2 behavior |
| --- | --- |
| `lifecycle` | No lifecycle-mode setting; use the leader or verifier binary for the intended role. |
| `validator.basefee` | No configurable ER base fee; V2 execution fees are zero. |
| Entire `[accountsdb]`: `database-size`, `block-size`, `index-size`, `max-snapshots`, `defragment-on-startup`, `reset` | Replaced by Engine storage. These values have no direct V2 equivalents. |
| `aperture.event-processors` | No corresponding tuning option. |
| `chainlink.max-monitored-accounts`, `secondary-max-monitored-accounts`, `remove-confined-accounts` | No direct replacement. `engine.accountsdb.lru-capacity` controls a different cache; don't copy these limits into it. |
| Entire `[chainlink.delegation-dedup]`, if configured | No corresponding V2 configuration section. |
| Entire `[task-scheduler]`: `reset`, `min-interval`, `failed-task-retention`, `failed-task-cleanup-interval` | Tasks use Hydra; run the separate cranker described in [cutover](#4-start-the-hydra-cranker). |
| Entire `[chain-operation]` and older `[admin]`, if present | Keep the existing domain record. Use [domain commands](../bins/magicblock/README.md#domain-records) for changes and schedule [fee claims](../bins/magicblock/README.md#fee-claims) externally. |

### Keep unchanged

Copy the existing values for:

- `remotes`, including provider credentials.
- `[metrics]` and `[grpc]` fields.
- `aperture.listen` and `aperture.geyser-plugins`; point plugin JSON at the rebuilt Yellowstone library.
- `commit.compute-unit-price`.
- `ledger.block-cache-size`, `ledger.verify-keypair`, and `ledger.reset` (false).
- `chainlink.allowed-programs`, `resubscription-delay`, `undelegation-request-poll-interval`, and `ws-subs-per-connection`.
- `chainlink.risk.enabled` and `chainlink.risk.request-timeout`.
- `[[programs]]` IDs and paths; install matching files on all nodes and add v42 as above.

### Add

| V2 setting | Value to supply |
| --- | --- |
| `engine.accountsdb.directory`, `engine.ledger.directory` | The two destination paths described above. |
| `engine.accountsdb.lru-capacity` | Optional recency-cache capacity; default `5000`. |
| `engine.blockstore.checkpoint` | Optional checksum interval in slots; default `0` disables it. |
| Leader `engine.replication.bind-address`, `allowed-followers` | Reachable TCP listener and verifier public-key allowlist. |
| Verifier `engine.replication.upstream-address`, `upstream-authority` | Leader TCP address and public key. Use independent verifier storage and identity; leave `engine.authority.remote` unset. |
| `chainlink.risk.risk-server-url` | Risk-server endpoint when risk checks are enabled. |
| `chainlink.risk.check-strategy` | Default `all-signers`; use `relevant-programs` only if that is the intended risk policy. |

Example leader storage/timing settings:

```toml
[engine.accountsdb]
directory = "/srv/magicblock-v2/accountsdb/migrated"

[engine.ledger]
directory = "/srv/magicblock-v2/ledger"
size-limit = 966367641600 # example: 900 GiB on the 1 TiB filesystem described above

[engine.blockstore]
blocktime = "50ms"
superblock = 72000       # one hour between seals at 50ms
checkpoint = 1200        # example: checksum checkpoint every minute at 50ms

[ledger]
reset = false
```

Choose a nonzero sealing interval and budget for the first snapshot before
verifier bootstrap. The migrated source slot is retained; the first seal occurs
at the next slot divisible by `superblock`, plus archive time.

`checkpoint` is the checksum interval in slots (`0` disables it). It detects
persisted-state divergence between seals; followers use the upstream records.
Checksumming pauses execution, so adjust the example cadence for your state size.
Checkpoints don't create snapshots or recovery points.
See [checkpoint validation](https://github.com/magicblock-labs/magicblock-engine/blob/0.23.1/replicator/README.md#stream-processing).

### Service environment and command line

Leader settings use `MBV_`; verifier settings use `MBV_VERIFIER_`. Nested fields
use `__`, and hyphens become underscores. For example,
`engine.blockstore.checkpoint` becomes `MBV_ENGINE__BLOCKSTORE__CHECKPOINT` on
the leader or `MBV_VERIFIER_ENGINE__BLOCKSTORE__CHECKPOINT` on a verifier.

| V1 environment variable | V2 leader replacement |
| --- | --- |
| `MBV_VALIDATOR__KEYPAIR` | `MBV_ENGINE__AUTHORITY__LOCAL` |
| `MBV_STORAGE` | `MBV_ENGINE__LEDGER__DIRECTORY` **and** `MBV_ENGINE__ACCOUNTSDB__DIRECTORY` |
| `MBV_LEDGER__BLOCK_TIME` | `MBV_ENGINE__BLOCKSTORE__BLOCKTIME` |
| `MBV_LEDGER__SUPERBLOCK_SIZE` | `MBV_ENGINE__BLOCKSTORE__SUPERBLOCK` |
| `MBV_LEDGER__SIZE` | `MBV_ENGINE__LEDGER__SIZE_LIMIT` |

Remove environment variables for every removed setting above, including the old
`MBV_VALIDATOR__*`, `MBV_ACCOUNTSDB__*`, `MBV_TASK_SCHEDULER__*`,
`MBV_CHAIN_OPERATION__*`, and `MBV_ADMIN__*` groups after transferring needed values.
Retained settings keep their existing leader variable names.

Both binaries take a positional config path. The leader supports `--remotes`,
`--listen`/`-l`, and `--metrics`/`-m`; put identity, storage, and timing in TOML or
the environment instead of V1 CLI overrides. The verifier takes only its config
path. Keep separate environment files for the two services.

Precedence is CLI → environment → TOML → defaults. See
[configuration overrides](../config/README.md#overrides).

### Operator jobs

Install `magicblock` alongside the validator. Fee and domain commands use
`--config /etc/magicblock/validator-v2.toml` to load the leader's identity and
base-chain RPC, including its `MBV_` environment overrides. Run them with the same
service user/environment as the leader.

**Fee claims:** replace `chain-operation.claim-fees-frequency` (or the old admin
claim schedule) with a cron job/systemd timer at the same cadence. Each invocation
claims once and exits:

```sh
/opt/magicblock/v2.0.0/magicblock claim-fees --config /etc/magicblock/validator-v2.toml
```

The leader still initializes fee vaults at startup, but doesn't claim automatically.
Claims use the leader identity as payer and recipient. A low vault balance is a
successful skip; logs distinguish skips from confirmed claims. See [fee claims](../bins/magicblock/README.md#fee-claims).

**Domain registration:** keep the existing record when the identity and advertised
values are unchanged. If the endpoint, block time, or fee metadata needs updating,
run `sync` before reopening ingress:

```sh
/opt/magicblock/v2.0.0/magicblock domain sync \
  --config /etc/magicblock/validator-v2.toml \
  --country-code US --fqdn https://validator.example.com
```

Use the deployment's country and endpoint. `sync` submits a full record with the
CLI's defaults for status, features, and load; it isn't an endpoint-only patch.
For an identity with no record, use `domain register` with the same arguments.
`domain unregister --config ...` removes a record and isn't part of the cutover.
See [domain commands](../bins/magicblock/README.md#domain-records).

**Health and failure monitoring:** `magicblock healthcheck` is a one-shot active
probe; `magicblock monitor` is the long-running failure subscription. Their
commands and service lifecycle are covered in [validation](#validation).

## Cutover

### 1. Stop V1

Drain ingress and stop the V1 leader and replicas. Keep them stopped. Retain the
complete source: AccountsDB, both service SQLite databases, and their WAL/SHM files
from the same stop.

### 2. Migrate

Substitute deployment paths and the base-chain RPC URL:

```sh
MIGRATOR=/opt/adb-migrator/1.4.0/adb-migrator
SOURCE=/home/magicblock/ledger
DESTINATION=/srv/magicblock-v2/accountsdb/migrated
BASE_RPC_URL=https://your-base-chain-rpc.example
SLOT_DURATION_MS=50

"$MIGRATOR" \
  --source "$SOURCE" \
  --destination "$DESTINATION" \
  --base-rpc-url "$BASE_RPC_URL" \
  --slot-duration-ms "$SLOT_DURATION_MS" \
  --dry-run > dry-run-report.txt 2> dry-run.log
```

`SOURCE` contains `accountsdb/main` and both SQLite databases; all three are
required. `DESTINATION` must not exist, but its parent must exist on the CoW mount.
Match `SLOT_DURATION_MS` to V2's block time. The RPC needs transaction history for
base-chain reconciliation.

Review the dry-run account/service counts and every `Unresolved intent` ID.
**Unresolved work may be omitted despite a successful exit.** Resolve or account
for those omissions before migrating.

Run without `--dry-run`:

```sh
"$MIGRATOR" \
  --source "$SOURCE" \
  --destination "$DESTINATION" \
  --base-rpc-url "$BASE_RPC_URL" \
  --slot-duration-ms "$SLOT_DURATION_MS" \
  > migration-report.txt 2> migration.log
```

Require exit status `0`, `Outcome: ✅ Passed`, and matching `Migration digest` /
`Verified digest` within the migration report. Review unresolved IDs again.
Dry-run and migration digests can differ because recovery timestamps differ.

The migration carries accounts, queued intents, active tasks as Hydra cranks, and
recoverable committor work. No separate SQLite task migration is needed. Recovery rules:

- Committor recovery uses a 14-day window; completed/expired groups are excluded.
- Cancelled/exhausted tasks and task failure history are excluded.
- Future task delays are preserved, with at least two destination slots before
  execution and no offline catch-up.

See the [migrator's recovery rules](https://github.com/magicblock-labs/adb-migrator/blob/1.4.0/README.md#recovery-behavior)
for reconciliation and sequence handling.

### 3. Link the legacy ledger and start V2

V2 reads legacy history at `<engine.ledger.directory>/rocksdb`:

```sh
mkdir -p /srv/magicblock-v2/ledger
ln -s /home/magicblock/ledger/rocksdb /srv/magicblock-v2/ledger/rocksdb
```

Keep `ledger.reset = false`. The link shares the original RocksDB files; V1 must
remain stopped while V2 uses them.

Start the leader through its service unit, with the positional config path:

```sh
/opt/magicblock/v2.0.0/magicblock-validator /etc/magicblock/validator-v2.toml
```

Keep ingress closed for validation. Startup can submit fee-vault setup transactions
and resume settlement/tasks immediately, before application traffic returns.

### 4. Start the Hydra cranker

Once V2's RPC is ready, start the cranker service. Its command/environment should be:

```sh
HYDRA_CRANKER_KEYPAIR=/etc/magicblock/cranker.json \
HYDRA_CRANKER_PROMETHEUS_PORT=9100 \
hydra-cranker --ephemeral \
  --rpc-url http://127.0.0.1:8899 \
  --ws-url ws://127.0.0.1:8900 \
  --base-rpc-url "$BASE_RPC_URL"
```

Run one for each ER. Scheduled tasks won't execute without it.

## Validation

Run the live failure monitor alongside the leader, starting and stopping it with
the validator service. Keep it enabled after migration and wait for its subscription
before sending validation transactions:

```sh
/opt/magicblock/v2.0.0/magicblock monitor --url http://127.0.0.1:8899
```

The command has no monitoring deadline and exits on WebSocket disconnection.
Configure automatic restart to handle RPC startup and reconnects, collect stderr
in the service logs, and propagate validator stops/restarts to the monitor.

It logs Engine loading/execution failures with signatures, slots, and errors.
Use `RUST_LOG=magicblock=debug` for instruction/account details and available
execution logs. Delivery is live-only and best-effort; check validator logs for
Chainlink and admission failures. See [monitor usage](../bins/magicblock/README.md#monitor-failures).

Then run the active healthcheck:

```sh
/opt/magicblock/v2.0.0/magicblock healthcheck \
  --url http://127.0.0.1:8899 --timeout 10s
```

This submits a v42 transaction and checks execution status plus signature/account
WebSocket notifications. `getHealth` only checks RPC reachability. For diagnostics,
use `RUST_LOG=magicblock=debug`. See the [healthcheck guide](../bins/magicblock/README.md#healthcheck).

- Run locally and through the public route. The probe derives WebSocket using the
  next port (`8899` → `8900`); account for proxy mappings.
- Compare saved V1 account samples: data, owners, balances, and delegation state.
- Query known V1 signatures/address history, then execute a small application
  transaction and verify its state changes and V2 history.
- Complete a delegation → execution → commit/undelegation flow on the base chain.
- Check recovered committor work, callbacks, and scheduled tasks.
  Confirm a migrated Hydra crank is triggered by the cranker.

## Replication

Give each verifier fresh, independent V2 storage and the same release/program
files as the leader. Old V1 replica databases aren't the bootstrap source.

| Process | Setting | Value |
| --- | --- | --- |
| Leader | `engine.replication.bind-address` | Reachable TCP listener, e.g. `10.0.0.10:10000` |
| Leader | `engine.replication.allowed-followers` | Verifier public keys; empty admits nobody |
| Verifier | `engine.authority.local` | Verifier's signing keypair |
| Verifier | `engine.replication.upstream-address` | Leader's TCP listener |
| Verifier | `engine.replication.upstream-authority` | Leader's public identity |
| Verifier | `engine.replication.allowed-followers` | `[]` for a leaf verifier |

Set the leader allowlist before startup. Leave `engine.authority.remote` unset
and synchronize host clocks.

Wait for a **completed leader snapshot**: a finished `accountsdb.tar.zst` in the
Engine ledger's superblock directories, not a `.tmp` file. Use the filesystem
archive as the completion signal, not `getHighestSnapshotSlot`. The snapshot
contains the migrated state needed to bootstrap the verifier.
See [follower recovery](https://github.com/magicblock-labs/magicblock-engine/blob/0.23.1/replicator/README.md#follower-recovery).

Start each verifier:

```sh
/opt/magicblock/v2.0.0/magicblock-verifier /etc/magicblock/verifier-v2.toml
```

Expect snapshot staging, an internal Engine restart, then a live stream. Check:

- `replicator_client_stream_connected = 1` and catch-up within the chosen lag limit.
- Agreement at a common sealed boundary: seal ID, transaction count, and
  persisted-state checksum; no replay/hash/seal mismatches.
- If checkpoints are enabled, no `checkpoint state mismatch` errors between seals.
- Known V2 writes present in verifier replay/state, and successful restart/resume.

Monitor verifiers through logs and metrics; they don't expose application RPC
or run settlement/tasks. Seal checksums cover persisted state, not volatile mirrors.

Reopen ingress after leader and required verifier checks pass. Watch settlement
recovery, task progress, replication lag/errors, and snapshot completion during
the initial observation period. See [metrics scraping](../metrics/README.md#scraping-metrics) and
[live failure monitoring](../bins/magicblock/README.md#monitor-failures).

## Retire legacy history

Keep the legacy ledger for a few days, then remove it manually once pre-cutover
history is no longer needed. During a brief leader stop, remove the `rocksdb`
symlink and its legacy target, leaving the V2 Engine ledger and migrated AccountsDB
intact. Restart the leader; it recreates an empty compatibility store.

Removing the legacy files releases filesystem usage against `size-limit` once
their open handles are closed. The freed budget becomes available for V2 history;
revisit the limit if it was raised temporarily for coexistence. Pre-cutover history
will no longer be available through V2.

## References

- [adb-migrator 1.4.0](https://github.com/magicblock-labs/adb-migrator/blob/1.4.0/README.md)
- [Hydra cranker deployment](https://github.com/magicblock-labs/magicblock-validator/issues/1815)
- [Yellowstone MBV 2.0 branch](https://github.com/magicblock-labs/yellowstone-grpc/tree/mbv-2.0.0) · [Plugin guide](https://github.com/magicblock-labs/yellowstone-grpc/blob/mbv-2.0.0/yellowstone-grpc-geyser/README.md) · [Sample config](https://github.com/magicblock-labs/yellowstone-grpc/blob/mbv-2.0.0/yellowstone-grpc-geyser/config.json)
- [v42 calculator](https://github.com/magicblock-labs/magicblock-engine/blob/0.23.2/programs/v42-calculator-program/README.md) · [Runtime program persistence](../runtime/README.md)
- [Leader](../bins/validator/README.md) · [Verifier](../bins/verifier/README.md)
- [Configuration](../config/README.md) · [Operator commands](../bins/magicblock/README.md)
- [Metrics](../metrics/README.md)
- [Engine 0.23.1 replication](https://github.com/magicblock-labs/magicblock-engine/blob/0.23.1/replicator/README.md)
