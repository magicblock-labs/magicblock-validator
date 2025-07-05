<div align="center">

  <img height="50x" src="https://magicblock-labs.github.io/README/img/magicblock-band.png" />


  <h1>Ephemeral Validator</h1>

  <p>
    <strong>Blazing-Fast SVM Ephemeral Validator: Clones accounts and programs just-in-time and settles state to a reference cluster.</strong>
  </p>

  <p>
    <a href="https://docs.magicblock.gg/Accelerate/ephemeral_rollups"><img alt="Documentation" src="https://img.shields.io/badge/docs-tutorials-blueviolet" /></a>
    <a href="https://github.com/magicblock-labs/bolt/issues"><img alt="Issues" src="https://img.shields.io/github/issues/magicblock-labs/magicblock-validator?color=blueviolet" /></a>
    <a href="https://discord.com/invite/MBkdC3gxcv"><img alt="Discord Chat" src="https://img.shields.io/discord/943797222162726962?color=blueviolet" /></a>
  </p>

</div>

## Overview

The Ephemeral Validator is a Solana Virtual Machine Validator that clones accounts and programs just-in-time and settles state to a reference cluster. 
It is designed to be used in a MagicBlock [Ephemeral Rollup](https://docs.magicblock.gg/introduction) instance to bring potentially anything on Solana, but can also be used as a super-charged [development](https://luzid.app/) environment.


## Ephemeral Rollups

Ephemeral Rollups extend Solana by enabling the Solana Virtual Machine (SVM) to replace centralized servers. They allow to use the SVM as a serverless, elastic compute for real-time use cases like gaming, finance, and DePIN, while keeping all smart contracts and state on Solana.

The core intuition is that by harnessing the SVM’s account structure and its capacity for parallelization, we can split the app/game state into shards. Users can lock one or multiple accounts to temporarily transfer the state to an auxiliary layer, which we define as the “ephemeral rollup”, a configurable dedicated runtime.

The Ephemeral Rollups instances always originate from a reference cluster, which is the source of truth for the state (programs and accounts). The session is eventually settled back to the reference cluster.

For the full documentation, please refer to the [Ephemeral Rollups](https://docs.magicblock.gg/Accelerate/ephemeral_rollups) page.

## Building

### **1. Install rustc, cargo and rustfmt.**

```bash
$ curl https://sh.rustup.rs -sSf | sh
$ source $HOME/.cargo/env
$ rustup component add rustfmt
```


### **2. Download the source code.**

```bash
$ git clone https://github.com/magicblock-labs/magicblock-validator.git
$ cd magicblock-validator
```

### **3. Build.**

```bash
$ cargo build
```

## Running the Ephemeral Validator

The validator supports configurations for the different use cases. The configuration files is a TOML file (some examples can be found in [configs](./configs)). Additionally, the configuration can be overridden by environment variables.

For example, to run the ephemeral validator on the devnet cluster, run:

```bash
$ cargo run -- configs/ephem-devnet.toml
```

Additionally, the validator can also be run with docker: [magicblocklabs/validator](https://hub.docker.com/r/magicblocklabs/validator)

## Testing

**Run the test suite:**

```bash
$ make test
```

## Integration Tests

**Running an integration test locally requires:**

### **1. Start a localnet cluster:**

```bash
$ cd test-integration
$ ./configs/run-test-validator.sh
```

### **2. Run the ephemeral validator:**

```bash
$ cargo run -- configs/ephem-localnet.toml
```

### **3. Run the integration test, e.g:**

```bash
$ cargo test --test 01_invocations test_schedule_commit_directly_with_single_ix --profile test
```

## Accessing the remote development cluster

* `ephemeral devnet` - stable public cluster for development accessible via
  https://devnet.magicblock.app. It uses solana devnet as base cluster for cloning and settling.

## Solana Program Runtime

- [Solana Program Runtime](https://docs.rs/solana-program-runtime/latest/solana_program_runtime/)

## Disclaimer

All claims, content, designs, algorithms, estimates, roadmaps,
specifications, and performance measurements described in this project
are done with the MagicBlock Labs, Pte. Ltd. (“ML”) good faith efforts. It is up to
the reader to check and validate their accuracy and truthfulness.
Furthermore, nothing in this project constitutes a solicitation for
investment.

Any content produced by ML or developer resources that ML provides are
for educational and inspirational purposes only. ML does not encourage,
induce or sanction the deployment, integration or use of any such
applications (including the code comprising the MagicBlock blockchain
protocol) in violation of applicable laws or regulations and hereby
prohibits any such deployment, integration or use. This includes the use of
any such applications by the reader (a) in violation of export control
or sanctions laws of the United States or any other applicable
jurisdiction, (b) if the reader is located in or ordinarily resident in
a country or territory subject to comprehensive sanctions administered
by the U.S. Office of Foreign Assets Control (OFAC), or (c) if the
reader is or is working on behalf of a Specially Designated National
(SDN) or a person subject to similar blocking or denied party
prohibitions.

The reader should be aware that U.S. export control and sanctions laws prohibit
U.S. persons (and other persons that are subject to such laws) from transacting
with persons in certain countries and territories or that are on the SDN list.
Accordingly, there is a risk to individuals that other persons using any of the
code contained in this repo, or a derivation thereof, may be sanctioned persons
and that transactions with such persons would be a violation of U.S. export
controls and sanctions law.

## Under construction

The Ephemeral Validator is in active development, so all APIs are subject to change. This code is unaudited. Use at your own risk.

## Open Source

Open Source is at the heart of what we do at Magicblock. We believe building software in the open, with thriving communities, helps leave the world a little better than we found it.
