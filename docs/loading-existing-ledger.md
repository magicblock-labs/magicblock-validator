## Loading Ledger on Startup

The below includes notes on how the ledger is loaded by solana validators. Since they support
bank forks and have multiple banks this is very different than our implementation.

### Loading Startup Config from Ledger Dir

On startup the validator expects to find all information to ininitialize itself inside the
ledger dir, for instance the `genesis.bin` file stores the `GenesisConfig`.
Keypairs for validator, faucet, stake account and vote account are stored in the respective `.json`
files there as well.

The validator logs are stored there as well inside `validator.log` and related rolling logs.

### Initializing Validator from Ledger

- `ledger/src/bank_forks_utils.rs` `load_bank_forks` uses a combination of snapshots and the
stored blockstore to get the bank initialized to slot 0
- the `core/src/validator.rs` calls this as part of its `load_blockstore` initialization method
- once that blockstore is loaded it is used to initialize a `ProcessBlockStore` which is then
used to _warp_ to a specific slot, see `core/src/validator.rs` `maybe_warp_to_slot`
