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

MagicBlock Validator hosts an **Ephemeral Rollup**: it brings base-chain accounts
into local state, accepts Solana-style transactions, and coordinates settlement
back to the base chain. [Engine](https://github.com/magicblock-labs/magicblock-engine)
owns execution, account storage, and replication; this workspace owns the
validator service graph and operator tools.

- **Leader:** application RPC/WebSocket endpoints, account synchronization,
  settlement, and repeated tasks.
- **Verifier:** follows an upstream replication stream with the same runtime
  image, without starting application services.
- **Operator clients:** explicit domain management, healthchecks, and a terminal
  monitor, independent of either host's lifecycle.

## Getting started

Use the Rust toolchain pinned in [rust-toolchain.toml](rust-toolchain.toml).
Formatting additionally uses nightly Rust. Native dependencies are required by
the storage and cryptography crates; consult the repository's
[build workflows](.github/workflows) for platform setup.

From the workspace root:

```bash
cargo build --release --locked \
  -p magicblock-validator -p magicblock-verifier \
  -p magicblock -p magicblock-validator-tui
```

Edit the [leader configuration](config.example.toml) before running: replace
sample identities and provider credentials, choose storage/listener addresses,
and supply the configured program ELF files.

```bash
cargo run --release --locked -p magicblock-validator -- --config config.example.toml
```

For a follower, configure the [verifier example](config.verifier.example.toml),
including the upstream's address/authority, follower allowlist, independent
storage, and matching program artifacts:

```bash
cargo run --release --locked -p magicblock-verifier -- config.verifier.example.toml
```

Leader settings use `MBV_` environment variables; verifier settings use
`MBV_VERIFIER_`. Nested keys use `__`. See
[configuration precedence and validation](magicblock-config/README.md).
Choose the process role through its binary, not a `lifecycle` setting.

Domain registration is explicit and can submit base-chain transactions; see the
[operator CLI](bins/magicblock/README.md). Use the
[TUI](bins/magicblock-validator-tui/README.md) to monitor a leader's RPC endpoints.
Both Engine-hosting binaries expose a separate combined MBV/Engine metrics
endpoint.

## Documentation

- [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md):
  project relationships, cross-chain contracts, deployment, and recovery.
- **Crate READMEs below:** local responsibilities, usage, and sharp edges.
  The same text is included in crate-level Rust documentation.
- [Contributing](docs/CONTRIBUTING.md) and [AGENTS.md](AGENTS.md):
  repository workflow and validation.

Generate local API documentation with `cargo doc --workspace --no-deps`.
Run the affected package's checks as described in AGENTS.md; the integration
and manual-test workspaces retain their own instructions.

## Workspace

### Processes

| Crate | Responsibility |
| --- | --- |
| [`magicblock-validator`](bins/magicblock-validator/README.md) | Leader, application services, and upstream replication. |
| [`magicblock-verifier`](bins/magicblock-verifier/README.md) | Follower replay, snapshot reopen, and optional relay. |
| [`magicblock`](bins/magicblock/README.md) | Domain operations and end-to-end healthchecks. |
| [`magicblock-validator-tui`](bins/magicblock-validator-tui/README.md) | External RPC/WebSocket terminal monitor. |

### Host services

| Crate | Responsibility |
| --- | --- |
| [`magicblock-runtime`](magicblock-runtime/README.md) | Shared native programs and startup account image. |
| [`magicblock-config`](magicblock-config/README.md) | Role-specific CLI, environment, and TOML configuration. |
| [`magicblock-aperture`](magicblock-aperture/README.md) | Application JSON-RPC, WebSocket subscriptions, and Geyser. |
| [`magicblock-chainlink`](magicblock-chainlink/README.md) | Base-chain account/program synchronization and materialization. |
| [`magicblock-aml`](magicblock-aml/README.md) | Risk-server client for activation checks. |
| [`magicblock-committor-service`](magicblock-committor-service/README.md) | Base-chain settlement preparation, delivery, and recovery. |
| [`magicblock-task-scheduler`](magicblock-task-scheduler/README.md) | Persistent repeated tasks and Engine crank submission. |
| [`magicblock-services`](magicblock-services/README.md) | Action callbacks and observed undelegation requests. |
| [`magicblock-validator-admin`](magicblock-validator-admin/README.md) | Periodic base-chain fee claims. |
| [`magicblock-metrics`](magicblock-metrics/README.md) | Validator collectors and combined metrics endpoint. |

### Programs and shared libraries

| Crate | Responsibility |
| --- | --- |
| [`magicblock-program`](programs/magicblock/README.md) | Native Magic, crank, callback, and ephemeral-system programs. |
| [`magicblock-magic-program-api`](magicblock-magic-program-api/README.md) | Shared IDs, instructions, PDAs, and response layouts. |
| [`magicblock-committor-program`](magicblock-committor-program/README.md) | Base-chain commit buffers and chunk tracking. |
| [`magicblock-table-mania`](magicblock-table-mania/README.md) | Base-chain address lookup table lifecycle. |
| [`magicblock-rpc-client`](magicblock-rpc-client/README.md) | Base-chain reads, submission, and confirmation. |
| [`magicblock-core`](magicblock-core/README.md) | Shared intent types, logging, and host utilities. |
| [`magicblock-version`](magicblock-version/README.md) | Build and compatibility metadata. |

### Legacy compatibility

| Crate | Responsibility |
| --- | --- |
| [`magicblock-ledger-deprecated`](magicblock-ledger/README.md) | Read-only historical RocksDB ledger. |
| [`solana-storage-proto`](storage-proto/README.md) | Legacy protobuf schemas and conversions. |

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
