<div align="center">
  <img height="100" src="https://magicblock-labs.github.io/README/img/magicblock-band.png" alt="MagicBlock Logo" />

  <h1>MagicBlock Validator</h1>

  <p>
    <strong>High-Performance SVM Validator for Ephemeral Rollups and Elastic Compute.</strong>
  </p>

  <p>
    <a href="https://docs.magicblock.gg"><img alt="Documentation" src="https://img.shields.io/badge/docs-tutorials-blueviolet" /></a>
    <a href="https://github.com/magicblock-labs/magicblock-validator/blob/main/LICENSE.md"><img alt="License" src="https://img.shields.io/badge/license-BSL--1.1-blue" /></a>
    <a href="https://discord.com/invite/MBkdC3gxcv"><img alt="Discord Chat" src="https://img.shields.io/discord/943797222162726962?color=blueviolet" /></a>
  </p>
</div>

---

## 🚧 Status: Under Construction

> **⚠️ Warning:** The Ephemeral Validator is in **active development**. All APIs are subject to change. This code is **unaudited**. Use at your own risk.

---

## 📖 Overview

The **MagicBlock Validator** is a specialized Solana Virtual Machine (SVM) runtime designed to power **Ephemeral Rollups**. It enables seamless scaling by cloning accounts and programs just-in-time from a reference cluster (like Solana Mainnet or Devnet), executing transactions in a high-performance environment, and settling state changes back to the base chain.

### Key Features
- **Ephemeral Rollups**: Offload compute to a dedicated layer while inheriting Solana's security and state.
- **Just-in-Time Cloning**: Automatically fetches accounts from a remote cluster when accessed.
- **State Settlement**: Batches and commits state transitions back to the reference chain.
- **Developer Friendly**: Can be used as a super-charged development environment compatible with standard Solana tooling.

## 🚀 Getting Started

### Prerequisites
- **Rust**: Latest stable and nightly toolchains.
- **Dependencies**: `solana-cli` (optional), `protobuf-compiler` (for gRPC support).

### Installation

1. **Clone the repository:**
   ```bash
   git clone [https://github.com/magicblock-labs/magicblock-validator.git](https://github.com/magicblock-labs/magicblock-validator.git)
   cd magicblock-validator

```

2. **Build the project:**
```bash
cargo build --release

```



## ⚙️ Configuration

The validator is highly configurable via TOML files or environment variables. A comprehensive reference configuration is available in [`config.example.toml`](https://www.google.com/search?q=./config.example.toml).

### Core Operational Modes (`lifecycle`)

The `lifecycle` setting determines how the validator manages state and syncing.

* **`ephemeral`** (**Currently Supported**): Clones accounts on demand from the remote cluster and writes changes only to delegated accounts. This is the primary mode for Ephemeral Rollups.
* *Note: Other modes (`replica`, `offline`) are present in the codebase but are currently experimental or unsupported.*

### Connecting to a Cluster

Configure the `remotes` list to specify where to fetch state from:

```toml
# Example: Sync with Solana Devnet
remotes = ["[https://api.devnet.solana.com](https://api.devnet.solana.com)", "wss://api.devnet.solana.com"]

```

## 🏃 Usage

### Running the Validator

To start the validator with the default configuration (or your custom config file):

```bash
cargo run --release -- config.example.toml

```

### Using Environment Variables

You can override any configuration value using environment variables with the `MBV_` prefix.

```bash
# Example: Run as an ephemeral validator syncing from Mainnet
MBV_LIFECYCLE=ephemeral \
MBV_LISTEN=0.0.0.0:8899 \
cargo run --release

```

### Docker

Official Docker images are available for streamlined deployment:

```bash
docker run -p 8899:8899 -p 8900:8900 magicblocklabs/validator

```

## ☁️ Remote Development Cluster

If you prefer not to run the validator locally, we provide a stable public cluster for development:

* **Endpoint**: `https://devnet.magicblock.app`
* **Base Cluster**: Solana Devnet

This cluster allows you to test Ephemeral Rollup interactions without local setup.

## 🧪 Testing

The project includes a comprehensive test suite managed via `Makefile`.

* **Run all tests (Unit & Integration):**
```bash
make test

```


* **Run integration tests specifically:**
See [test-integration/README.md](https://www.google.com/search?q=./test-integration/README.md) for detailed instructions on running specific scenarios.
```bash
make -C test-integration test

```



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

This project is licensed under the **Business Source License 1.1**. See [LICENSE.md](https://www.google.com/search?q=./LICENSE.md) for details.

---

<div align="center">
<sub>Built with ❤️ by MagicBlock Labs</sub>
</div>

