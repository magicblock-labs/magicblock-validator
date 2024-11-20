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

`ledger/src/blockstore_processor.rs` `process_entries`:

- given bankd (with scheduler), replay entries
- iterates through replay entries
- defers processing of ticks, but processes all batches collected up to this point each time a
  tick is encountered and then clears the batches
- processes transactions found in an entry immediately via the _normal_ route
  - first prepares a batch of transactions via `bank.prepare_sanitized_batch`
  - locking accounts for the batch could work and in this cases it is added to `batches` and
    we're done with the entry
  - if not we process the current batches `process_batches`, clear them and
    then try to process the entry again
- once we iterated through all entries we process the remaining batches `process_batches`
  and then register each tick hash with the bank `bank.register_tick(hash)`

`ledger/src/blockstore_processor.rs` `process_batches`:

- given bank (with scheduler), batches
- depending on if `bank.has_installed_scheduler()` it either calls
  A) `schedule_batches_for_execution` or B) `rebatch_and_execute_batches`

Investigating when each of these branches is taken I ran this with the agave test validator,
loading an existing ledger:


First **B)**: is called multiple times with the following stack:

```
solana_ledger::blockstore_processor::process_batches blockstore_processor.rs:408
solana_ledger::blockstore_processor::process_entries blockstore_processor.rs:664
solana_ledger::blockstore_processor::confirm_slot_entries blockstore_processor.rs:1696
solana_ledger::blockstore_processor::confirm_slot blockstore_processor.rs:1533
solana_ledger::blockstore_processor::confirm_full_slot blockstore_processor.rs:1164
solana_ledger::blockstore_processor::process_bank_0 blockstore_processor.rs:1765
solana_ledger::blockstore_processor::process_blockstore_for_bank_0 blockstore_processor.rs:956
solana_ledger::bank_forks_utils::load_bank_forks bank_forks_utils.rs:189
solana_core::validator::load_blockstore validator.rs:1978
solana_core::validator::Validator::new validator.rs:726
solana_test_validator::TestValidator::start lib.rs:1037
solana_test_validator::TestValidatorGenesis::start_with_mint_address_and_geyser_plugin_rpc lib.rs:625
solana_test_validator::main solana-test-validator.rs:574
```

Much later **A)**: is called with the following stack:

```
solana_ledger::blockstore_processor::process_batches blockstore_processor.rs:380
solana_ledger::blockstore_processor::process_entries blockstore_processor.rs:664
solana_ledger::blockstore_processor::confirm_slot_entries blockstore_processor.rs:1696
solana_ledger::blockstore_processor::confirm_slot blockstore_processor.rs:1533
solana_ledger::blockstore_processor::confirm_full_slot blockstore_processor.rs:1164
solana_ledger::blockstore_processor::process_single_slot blockstore_processor.rs:2144
solana_ledger::blockstore_processor::load_frozen_forks blockstore_processor.rs:1936
solana_ledger::blockstore_processor::process_blockstore_from_root blockstore_processor.rs:1038
solana_core::validator::ProcessBlockStore::process validator.rs:2103
solana_core::validator::ProcessBlockStore::process_to_create_tower validator.rs:2164
solana_core::validator::Validator::new validator.rs:1346
solana_test_validator::TestValidator::start lib.rs:1037
solana_test_validator::TestValidatorGenesis::start_with_mint_address_and_geyser_plugin_rpc lib.rs:625
solana_test_validator::main solana-test-validator.rs:574
```

`ledger/src/blockstore_processor.rs` `schedule_batches_for_execution`:

- given bank (with scheduler), batches array
- schedules executions of the transactions in each batch via
  `bank.schedule_transaction_executions`

`runtime/src/installed_scheduler_pool.rs` `schedule_transaction_executions`:

- given transactions_with_indexes iterator
- schedules execution of each sanitized transaction with the included scheduler, in our case
  the `PooledScheduler::schedule_execution` which then creates and sends a Task to execute the
  transaction
- this then ends up executing the transaciont on the `scheduler_main_loop`
