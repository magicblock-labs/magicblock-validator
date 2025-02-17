## genx

This is a tool to be used to generate configs/scripts, etc.

### test-validator

Used to generate a test-validator script that makes it pre-load accounts from chain as follows:

1. Find all accounts stored in the ledger (provided via the first arg)
2. Add them to the script as `--maybe-clone` to have them loaded into the validator on startup
if they exist on chain
3. Save the script in a tmp folder and print its path to the terminal

```sh
cargo run --release --bin genx test-validator \
  --rpc-port 7799 --url 'https://rpc.magicblock.app/mainnet' \
 ledger
```

Then you run the script to start a mainnet standin locally.

Also make sure to update the config with which to start the ephemeral validator that will
replay the ledger to point to the port that it is running on.

#### Config Example

```toml
[accounts]
remote = ["http://127.0.0.1:7799", "ws://127.0.0.1:7800"]
lifecycle = "ephemeral"
commit = { frequency_millis = 50_000 }
payer = { init_sol = 1_000 }

[rpc]
addr = "0.0.0.0"
port = 8899

[validator]
millis_per_slot = 50

[ledger]
reset = false
path = "./ledger"
```

### test-ledger

Prepares a ledger for replay in a diagnostics/test scenario. At this point it ensures that the
original keypair of the validator operator is not used. Instead it is overwritten with a random
keypair. That keypair is then printed to the terminal so that the `VALIDATOR_KEYPAIR` env var
can be set to it when running the epehemeral validator to replay the ledger.

One can auto-set that env var using the output of this command as follows:

```sh
export VALIDATOR_KEYPAIR=$(cargo run --bin genx -- test-ledger ./ledger)
```

Then we can just do the following to replay the ledger (i.e. providing the above config):

```sh
cargo run --release ledger-config.toml
```

