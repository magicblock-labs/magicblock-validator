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
  test-integration/ledgers/ledgers
```
