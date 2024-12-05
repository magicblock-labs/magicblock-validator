## Summary

The `Bank` is responsible for holding account states and preparing transactions
that are then executed inside the SVM. The SVM is implemented in its own crate.
The `Bank` also does post processing to update state after the transaction ran inside the SVM

## Details

*Important symbols:*

- `Bank` struct
  - Basically contains a full SVM chain state
  - It's basically a fully fledged solana client with all utils (Fees/Logs/Slots/Rent/Cost)
  - Contains a `BankRc` which is just a `Arc<Accounts>`
  - make it possible to share the accounts db across threads
  - Contains a `StatusCache`
  - Uses `TransactionBatchProcessor` for simulating and executing transactions
  - Shares a `LoadedPrograms` with the transaction processor


- `StatusCache` struct
  - It's basically a `HashMap<Hash, (Slot, HashMap<Key, Vec<(Slot, T)>>)>`
  - // TODO(vbrunet) - figure out exactly how data structure works

### Builtin Programs

We support and load the following builtin programs at startup:

- `system_program`
- `solana_bpf_loader_upgradeable_program`
- `compute_budget_program`
- `address_lookup_table_program`
- `magicblock_program` which supports account mutations, etc.

We don't support the following builtin programs:

- `vote_program` since we have no votes
- `stake_program` since we don't support staking in our validator
- `config_program` since we don't support configuration (_Add configuration data to the chain and the
list of public keys that are permitted to modify it_)
- `solana_bpf_loader_deprecated_program` because it's deprecated
- `solana_bpf_loader_program` since we use the `solana_bpf_loader_upgradeable_program` instead
- `zk_token_proof_program` it's behind a feature flag (`feature_set::zk_token_sdk_enabled`) in
  the solana validator and we don't support it yet
- `solana_sdk::loader_v4` it's behind a feature flag (`feature_set::enable_program_runtime_v2_and_loader_v4`) in the solana
  validator and we don't support it yet

## Notes

`Bank` implements `AddressLoader`, used to sanitize transactions.

*Important dependencies:*

- Provides `Accounts`: [solana/accounts-db](../solana/accounts-db/README.md)
- Provides `TransactionBatchProcessor`: [solana/svm](../solana/svm/README.md)
- Provides `LoadedPrograms`: [solana/program-runtime](../solana/program-runtime/README.md)
