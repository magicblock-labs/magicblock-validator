# @magicblock-labs/ephemeral-validator

Released binaries for the MagicBlock ephemeral rollup stack, exposed as commands.

## Commands

| Command                   | What it runs                                                                     |
| ------------------------- | -------------------------------------------------------------------------------- |
| `mb-stack`                | **The whole local stack, wired together** (see below)                            |
| `ephemeral-validator`     | The ephemeral rollup validator                                                   |
| `query-filtering-service` | The RPC query-filtering front                                                    |
| `mb-test-validator`       | A base-layer `solana-test-validator` preloaded with MagicBlock programs/accounts |
| `rpc-router`              | RPC routing frontend                                                             |
| `vrf-oracle`              | VRF oracle                                                                       |
| `hydra-cranker`           | Permissionless Hydra crank runner (fires scheduled tasks)                        |

Run any of them with `npx`, e.g. `npx @magicblock-labs/ephemeral-validator mb-stack`.

## `mb-stack`

Boots the full local stack as one supervised process, already connected:

```bash
client ──► query-filtering-service ──► ephemeral-validator ──► mb-test-validator (base L1)
           http 6699 / ws 6700         http 7799 / ws 7800      http 8899 / ws 8900
           (public entry)              (internal)               (internal)
```

Pass `--cranker` to also supervise a `hydra-cranker` pointed at the ephemeral
validator, so scheduled tasks are actually triggered locally:

```bash
MB_STACK_CRANKER_KEYPAIR=~/.config/solana/cranker.json mb-stack --cranker
```

`MB_STACK_CRANKER_KEYPAIR` is required with `--cranker`.
It accepts a keypair file path, a 64-byte JSON array, or a base58 keypair.
The cranker signs and pays for its `Trigger` transactions (and receives the
per-trigger reward), so that account must already exist and be funded on the ER.

Any other extra args you pass to `mb-stack` are forwarded to the **mb-test-validator**
(base L1), e.g. `mb-stack --bpf-program <id> <path.so> --account <pk> <file> --reset`.
(Change ports via the env vars below, not `--rpc-port`, which `solana-test-validator`
rejects if passed twice. When `MB_STACK_ER_REMOTES` is set the base is skipped, so
these args have no effect.)

Point your client at the **public RPC endpoint `http://127.0.0.1:6699`**. Requests
are filtered by the query-filtering-service, served by the ephemeral validator,
which in turn clones from / commits to the base validator.

Services start in order and each is health-gated before the next begins. If any
service exits, the whole stack is torn down; `Ctrl-C` (or `SIGTERM`) shuts
everything down cleanly. Each service's output is prefixed with a colored tag
(`base`, `er`, `qfs`) so the three logs stay readable instead of interleaving.

### Port overrides

The three public-facing ports can be overridden via environment variables
(WebSocket port is always RPC port + 1):

| Variable                   | Default                       | Service                                             |
| -------------------------- | ----------------------------- | --------------------------------------------------- |
| `MB_STACK_PUBLIC_PORT`     | `6699`                        | query-filtering-service (public entry)              |
| `MB_STACK_ER_PORT`         | `7799`                        | ephemeral-validator                                 |
| `MB_STACK_BASE_PORT`       | `8899`                        | base validator                                      |
| `MB_STACK_ER_REMOTES`      | _(local base)_                | comma-separated base-chain remotes for the ER       |
| `MB_STACK_CRANKER_PORT`    | `9797`                        | hydra-cranker Prometheus/health port (`--cranker`)  |
| `MB_STACK_CRANKER_KEYPAIR` | _(required with `--cranker`)_ | funded keypair the cranker signs `Trigger` txs with |

Setting `MB_STACK_ER_REMOTES` (e.g. `MB_STACK_ER_REMOTES=devnet mb-stack`) points the
ephemeral-validator at an external base chain and **skips** the local base
validator. For anything more specific, run the individual commands directly.

> `mb-test-validator` requires `solana-test-validator` on `PATH`.

### Base-chain setup

The ephemeral-validator performs a mandatory on-chain "magic fee vault" init
against its base chain on startup. The bundled `mb-test-validator` preloads the
MagicBlock programs and accounts needed for this local flow.

`solana-test-validator` ignores `--bpf-program`, `--account`, and faucet genesis
arguments when its ledger already exists. If you previously started the stack
with stale local dumps, restart with `mb-stack --reset` or delete the local
`test-ledger` so the base validator rebuilds genesis with the bundled dumps.
