# @magicblock-labs/ephemeral-validator

Run a local MagicBlock stack or its individual services using packaged binaries.

## Start a local stack

Install `solana-test-validator` and make sure it is on your `PATH`, then run:

```bash
npx --package @magicblock-labs/ephemeral-validator mb-stack
```

Connect your application to **http://127.0.0.1:6699** (WebSocket:
**ws://127.0.0.1:6700**). The stack starts a base-chain test validator, an
ephemeral validator, and a query-filtering service.

Pass `--cranker` to also supervise a `hydra-cranker` pointed at the ephemeral
validator, so scheduled tasks are actually triggered locally:

```bash
MB_STACK_CRANKER_KEYPAIR=~/.config/solana/cranker.json npx --package @magicblock-labs/ephemeral-validator mb-stack --cranker
```

`MB_STACK_CRANKER_KEYPAIR` is required with `--cranker`.
It accepts a keypair file path, a 64-byte JSON array, or a base58 keypair.
The cranker signs and pays for its `Trigger` transactions (and receives the
per-trigger reward), so that account must already exist and be funded on the ER.

Press Ctrl-C to stop everything. If a service exits, the stack stops the other
services too.

## Customize the stack

| Environment variable | Purpose | Default |
| --- | --- | --- |
| `MB_STACK_PUBLIC_PORT` | Application RPC port | `6699` |
| `MB_STACK_ER_PORT` | Ephemeral validator RPC port | `7799` |
| `MB_STACK_BASE_PORT` | Base-chain test validator RPC port | `8899` |
| `MB_STACK_ER_REMOTES` | External base-chain remotes, comma-separated | Local test validator |
| `MB_STACK_CRANKER_PORT` | hydra-cranker Prometheus/health port (`--cranker`) | `9797` |
| `MB_STACK_CRANKER_KEYPAIR` | Funded keypair the cranker signs `Trigger` txs with | required with `--cranker` |

Each WebSocket port is its RPC port plus one. Setting `MB_STACK_ER_REMOTES`
skips the local base-chain validator; the external chain must have the required
MagicBlock programs and accounts.

Extra arguments go to the base-chain test validator, for example:

```bash
npx --package @magicblock-labs/ephemeral-validator mb-stack --reset
```

Use the environment variables to change ports, not `--rpc-port`. Extra base-chain
arguments have no effect when using external remotes.

The local base chain loads bundled MagicBlock programs and accounts only when
creating its ledger. If you need to reload them, `--reset` rebuilds that ledger
and discards its existing state.

## Individual commands

Run any command with `npx --package @magicblock-labs/ephemeral-validator <command>`.

| Command | Purpose |
| --- | --- |
| `ephemeral-validator` | Run the ephemeral validator |
| `mb-test-validator` | Run a base-chain test validator with MagicBlock programs |
| `query-filtering-service` | Run the RPC query-filtering service |
| `rpc-router` | Run the RPC routing service |
| `vrf-oracle` | Run the VRF oracle |
| `hydra-cranker` | Run the permissionless Hydra crank runner |

Use individual commands when you need configuration beyond the stack overrides.
