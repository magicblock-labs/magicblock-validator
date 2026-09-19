<div align="center">
  <img height="100" src="https://magicblock-labs.github.io/README/img/magicblock-band.png" alt="MagicBlock Logo" />
  <h1>MagicBlock Validator</h1>
  <p><b>Blazing-fast SVM execution for real-time applications. Powered by Ephemeral Rollups.</b></p>
  <p>
    <a href="https://docs.magicblock.gg"><img alt="Documentation" src="https://img.shields.io/badge/docs-tutorials-blueviolet" /></a>
    <a href="LICENSE.md"><img alt="License BSL-1.1" src="https://img.shields.io/badge/license-BSL--1.1-blue" /></a>
    <a href="https://discord.com/invite/MBkdC3gxcv"><img alt="Discord Chat" src="https://img.shields.io/discord/943797222162726962?color=blueviolet" /></a>
  </p>
  <p>
    <a href="https://docs.magicblock.gg">Build with MagicBlock</a> ·
    <a href="#-try-it-locally">Try it locally</a> ·
    <a href="bins/magicblock-validator/README.md">Run a validator</a>
  </p>
</div>

---

MagicBlock Validator brings **Ephemeral Rollups** to Solana applications. Delegate
accounts to a rollup, execute against that state, and commit updates back to
Solana, keeping the Solana program and account model at the center of your app.

Build for interactions that happen continuously: game moves, shared worlds,
and other stateful experiences. Use the rollup for those interactions while
keeping your application's state connected to Solana through synchronization
and settlement.

## ✨ What you can build on

| | |
| :-- | :-- |
| **⚡ Solana execution** — run Solana programs on the SVM, powered by [MagicBlock Engine](https://github.com/magicblock-labs/magicblock-engine). | **🔗 Connected state** — bring base-chain accounts into the rollup and commit delegated state back to Solana. |
| **📡 Live applications** — submit transactions over RPC and follow account changes, logs, and transaction updates over WebSocket. | **⏱️ Recurring actions** — schedule application work to run at intervals, without a client sending every transaction. |
| **🔁 Replicated execution** — run verifiers that follow and replay a leader's transaction stream. | **🛠️ Tools included** — develop locally with a packaged stack, inspect a running leader in the terminal, and monitor it with Prometheus. |

### Build with both delegated and rollup-local state

Bring existing Solana accounts into the rollup, or create **sponsored ephemeral
accounts** for state your application needs locally. Programs can create, resize,
and explicitly close these accounts, with a sponsor funding their backing.
That gives applications room for session state and intermediate results without
making every account part of the base-chain settlement flow. See the
[native program guide](programs/magicblock/README.md) for the available operations.

### Keep the application moving between user interactions

**Recurring tasks** let applications schedule instructions at intervals rather
than relying on a connected client to submit each transaction. Tasks persist
across restarts and can be replaced or cancelled by their authority. They suit
periodic application work, rather than exact wall-clock deadlines.

Applications can also request **base-chain actions and local callbacks**, letting
rollup logic react to an action's result. The action and callback are separate
transactions, not one atomic cross-chain operation. Explore
[scheduled tasks](magicblock-task-scheduler/README.md) and
[callbacks](magicblock-services/README.md#callbacks).

### Connect clients and stream live activity

Use **Solana-compatible RPC API** to submit or simulate transactions and read accounts
and transaction history. WebSocket subscriptions keep clients informed of account
changes, logs, and transaction status without continuous polling.

For deeper integrations, **Geyser plugins** can feed validator notifications into
external systems such as indexers. Plugins must match the validator's Rust and
Agave ABI; the available notification fields differ from a full Solana validator.
See the [RPC and plugin guide](magicblock-aperture/README.md).

## 🌉 From Solana to a rollup and back

**Delegate → Execute → Commit**

Delegation makes selected accounts available for rollup execution. Your app
submits transactions to the rollup and follows its live state. Commits deliver
state updates to the base chain; undelegation returns control of those accounts
to Solana.

Rollup execution and base-chain settlement are separate steps. Start with the
[developer documentation](https://docs.magicblock.gg) for the application flow;
the [settlement guide](magicblock-committor-service/README.md) explains completion
and retry boundaries.

## 🚀 Try it locally

Start a local development stack with the packaged validator. Install
`solana-test-validator` and make it available on your `PATH`, then run:

```bash
npx --package @magicblock-labs/ephemeral-validator mb-stack
```

Connect your application to **http://127.0.0.1:6699** for RPC and
**ws://127.0.0.1:6700** for subscriptions. The stack starts a local base chain,
an Ephemeral Rollup validator, and a query-filtering service. Press Ctrl-C to
stop it.

See the [local stack guide](.github/packages/npm-package/README.md) for custom
ports, external remotes, and individual services.

## 🧭 Go further

| I want to… | Start here |
| :-- | :-- |
| Build an application | [Developer documentation](https://docs.magicblock.gg) |
| Run a leader from source | [Validator setup](bins/magicblock-validator/README.md) |
| Follow a leader with a verifier | [Verifier setup](bins/magicblock-verifier/README.md) |
| Configure a deployment | [Configuration guide](magicblock-config/README.md) |
| Manage a validator | [Operator CLI](bins/magicblock/README.md) · [Terminal monitor](bins/magicblock-validator-tui/README.md) · [Metrics](magicblock-metrics/README.md) |
| Understand the system | [Architecture and operations](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md) |

## 🦀 Inside the project

[MagicBlock Engine](https://github.com/magicblock-labs/magicblock-engine) runs
transactions and stores their results. To explore the surrounding services,
start with the part of the application flow that interests you:

- **Bring accounts into the rollup:** [Chainlink](magicblock-chainlink/README.md).
- **Submit transactions and follow updates:** [Aperture](magicblock-aperture/README.md).
- **Commit state back to Solana:** [Committor](magicblock-committor-service/README.md).
- **Run recurring actions:** [task scheduler](magicblock-task-scheduler/README.md).
- **Build rollup instructions:** [native programs](programs/magicblock/README.md)
  and the [instruction API](magicblock-magic-program-api/README.md).

Want to contribute? Start with [contributing](docs/CONTRIBUTING.md) for development
setup and checks, or explore the supported [integration tests](test-integration/).
Maintainers can follow the [release process](docs/RELEASE_PROCESS.md).

## 🔒 API stability and security

The Ephemeral Validator remains under active development, but its public,
application-facing APIs have matured. Breaking changes to supported APIs are
expected to be infrequent and will be clearly communicated in release notes.

The Delegation Program, the on-chain contract governing delegation, settlement,
and state commitment, has been independently audited. The validator internals
have been battle-tested, but the complete validator codebase and all internal
components have not undergone a comprehensive audit. Use at your own risk.

Internal interfaces explicitly marked experimental or unsupported may still change.

Report vulnerabilities through the [security policy](docs/SECURITY.md), not public issues.

## 📄 License and disclaimer

Licensed under the **Business Source License 1.1**; see [LICENSE.md](LICENSE.md).
Read the [disclaimer and export-control terms](docs/DISCLAIMER.md).

---

<p align="center"><sub>Built with ❤️ by MagicBlock Labs</sub></p>
