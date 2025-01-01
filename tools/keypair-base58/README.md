## keypair-base58

Prints the keypair in base58 format for a keypair file.

Example usage to set the `VALIDATOR_KEYPAIR` environment variable before starting up the
validator that is replaying a ledger for a validator with that keypair:

```sh
export VALIDATOR_KEYPAIR=`cargo run --bin keypair-base58 -- ledger/validator-keypair.json`
```
