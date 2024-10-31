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
- once that blockstore is loaded it is used to initialize a `ProcessBlockStore`
- the `ProcessBlockStore` is then used to optionally _warp_ to a specific slot, see
`core/src/validator.rs` `maybe_warp_to_slot`
- more importantly its `process` method is responsible for processing transactions in the
ledger to get the bank, etc. into the correct state

### Ledger `process_blockstore_from_root`

`ledger/src/blockstore_processor.rs` `process_blockstore_from_root` is responsible for the
following:

- given a blockstore, bank_forks
- extracts `start_slot` and `start_slot_hash` from the root bank found in the `bank_forks`
- determines the `highest_slot` of the blockstore
- ensures start_slot is rooted for correct replay
- calls into `load_frozen_forks`

`ledger/src/blockstore_processor.rs` `load_frozen_forks`:

- given bank_forks, start_slot_meta and blockstore
- prepares `pending_slots` via a call to `process_next_slots`
- `pending_slots` are then consumed one by one via `process_single_slot`

`ledger/src/blockstore_processor.rs` `process_single_slot`:

- given blockstore and bank (with scheduler)
- calls into `confirm_full_slot`, then freezes the `bank` and inserts its hash into the
`blockstore`

`ledger/src/blockstore_processor.rs` `confirm_full_slot`:

- given blockstore and bank (with scheduler)
- calls into `confirm_slot` and checks if `bank` _completed_ properly

`ledger/src/blockstore_processor.rs` `confirm_slot`:

- given blockstore and bank (with scheduler)
- obtains the `slot_entries` for the `bank.slot()` from the blockstore via
`blockstore.get_slot_entries_with_shred_info`
- passes them to `confirm_slot_entries`

`ledger/src/blockstore_processor.rs` `confirm_slot_entries`:

- given bank (with scheduler), slot entries
- finds + counts transactions inside the slot entries
- optionally verifies that a segment of entries has the correct number of ticks and hashes
- optionally verifies transactions for each entry
- creates `ReplayEntry`s (either a tick or vec of transactions)
- passes them to `process_entries`
