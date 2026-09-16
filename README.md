<div align="center">
  <img height="100" src="https://magicblock-labs.github.io/README/img/magicblock-band.png" alt="MagicBlock Logo" />

  <h1>MagicBlock Validator</h1>

  <p>
    <strong>Blazing Fast SVM Validator for Ephemeral Rollups and Elastic Compute.</strong>
  </p>

  <p>
    <a href="https://docs.magicblock.gg"><img alt="Documentation" src="https://img.shields.io/badge/docs-tutorials-blueviolet" /></a>
    <a href="LICENSE.md"><img alt="License" src="https://img.shields.io/badge/license-BSL--1.1-blue" /></a>
    <a href="https://discord.com/invite/MBkdC3gxcv"><img alt="Discord Chat" src="https://img.shields.io/discord/943797222162726962?color=blueviolet" /></a>
  </p>
</div>

## Overview

MagicBlock Validator runs an **Ephemeral Rollup**: applications use Solana-style
transactions against local accounts, with state settled back to the base chain.

- The **leader** serves application RPC and coordinates account synchronization,
  settlement, and recurring tasks.
- A **verifier** follows replication without running application services.
- **Operator tools** manage domain records, check health, and monitor a leader.

[Engine](https://github.com/magicblock-labs/magicblock-engine) provides transaction
execution, account storage, and replication.

## Getting started

Use the Rust toolchain in [rust-toolchain.toml](rust-toolchain.toml). For native
build dependencies, see the [build workflows](.github/workflows/).

Choose a process and follow its setup guide:

| Goal | Guide |
| --- | --- |
| Run a leader | [Validator](bins/magicblock-validator/README.md) |
| Follow a leader | [Verifier](bins/magicblock-verifier/README.md) |
| Manage domain records or check health | [Operator CLI](bins/magicblock/README.md) |
| Monitor a leader in the terminal | [TUI](bins/magicblock-validator-tui/README.md) |
| Run a packaged local development stack | [npm package](.github/packages/npm-package/README.md) |

For a leader, prepare `config.toml` using the
[example configuration](config.example.toml), then run from the workspace root:

```bash
cargo run --release --locked -p magicblock-validator -- --config config.toml
```

Replace sample identities, provider credentials, addresses, and program paths
before starting. See [configuration](magicblock-config/README.md) for environment
overrides and the separate verifier configuration.

## Find your way around

Component guides explain each crate's purpose and important usage constraints.

| Area | Components |
| --- | --- |
| Runtime and configuration | [Runtime image](magicblock-runtime/README.md), [configuration](magicblock-config/README.md) |
| Application access | [RPC and subscriptions](magicblock-aperture/README.md) |
| Account synchronization | [Chainlink](magicblock-chainlink/README.md), [risk checks](magicblock-aml/README.md) |
| Settlement | [Delivery](magicblock-committor-service/README.md), [buffers](magicblock-committor-program/README.md), [lookup tables](magicblock-table-mania/README.md) |
| Background work | [Recurring tasks](magicblock-task-scheduler/README.md), [callbacks and undelegation](magicblock-services/README.md) |
| Fee claims | [One-shot operator command](bins/magicblock/README.md#fee-claims) |
| Programs | [Native programs](programs/magicblock/README.md), [instruction API](magicblock-magic-program-api/README.md) |
| Shared utilities | [Core types](magicblock-core/README.md), [base-chain RPC client](magicblock-rpc-client/README.md), [metrics](magicblock-metrics/README.md), [version metadata](magicblock-version/README.md) |
| Legacy history | [Read-only ledger](magicblock-ledger/README.md), [storage schemas](storage-proto/README.md) |

## Contributing and further reading

- [Contributing](docs/CONTRIBUTING.md) and [repository guidance](AGENTS.md):
  development workflow and checks.
- [Manual tests](test-manual/README.md): integrations needing external setup.
- [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md):
  architecture, deployment, and recovery.

Generate API documentation with `cargo doc --workspace --no-deps`.

## API Stability and Security

The Ephemeral Validator remains under active development, but its public, application-facing APIs have matured. Breaking changes to supported APIs are expected to be infrequent and will be clearly communicated in release notes.

The Delegation Program—the on-chain contract governing delegation, settlement, and state commitment—has been independently audited. The validator internals have been battle-tested, but the complete validator codebase and all internal components have not undergone a comprehensive audit. Use at your own risk.

Internal interfaces explicitly marked experimental or unsupported may still change.

## ⚖️ Disclaimer

All claims, content, designs, algorithms, estimates, roadmaps, specifications, and performance measurements described in this project are done with MagicBlock Labs, Pte. Ltd. (“ML”) good faith efforts. It is up to the reader to check and validate their accuracy and truthfulness. Furthermore, nothing in this project constitutes a solicitation for investment.

Any content produced by ML or developer resources that ML provides are for educational and inspirational purposes only. ML does not encourage, induce or sanction the deployment, integration or use of any such applications (including the code comprising the MagicBlock blockchain protocol) in violation of applicable laws or regulations and hereby prohibits any such deployment, integration or use.

**Export Controls & Sanctions**
This includes the use of any such applications by the reader:
(a) in violation of export control or sanctions laws of the United States or any other applicable jurisdiction;
(b) if the reader is located in or ordinarily resident in a country or territory subject to comprehensive sanctions administered by the U.S. Office of Foreign Assets Control (OFAC); or
(c) if the reader is or is working on behalf of a Specially Designated National (SDN) or a person subject to similar blocking or denied party prohibitions.

The reader should be aware that U.S. export control and sanctions laws prohibit U.S. persons (and other persons that are subject to such laws) from transacting with persons in certain countries and territories or that are on the SDN list. Accordingly, there is a risk to individuals that other persons using any of the code contained in this repo, or a derivation thereof, may be sanctioned persons and that transactions with such persons would be a violation of U.S. export controls and sanctions law.

## ❤️ Open Source

Open Source is at the heart of what we do at MagicBlock. We believe building software in the open, with thriving communities, helps leave the world a little better than we found it.

## 📄 License

This project is licensed under the **Business Source License 1.1**. See [LICENSE.md](./LICENSE.md) for details.


---

<div align="center">
<sub>Built with ❤️ by MagicBlock Labs</sub>
</div>
