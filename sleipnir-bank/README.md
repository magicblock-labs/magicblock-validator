
# Summary

The `Bank` is responsible for holding account states and preparing transactions
that are then executed inside the SVM. The SVM is implemented in its own crate.
The `Bank` also does post processing to update state after the transaction ran inside the SVM

# Details

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

# Notes

`Bank` implements `AddressLoader`, used to sanitize transactions.

*Important dependencies:*

- Provides `Accounts`: [solana/accounts-db](../solana/accounts-db/README.md)
- Provides `TransactionBatchProcessor`: [solana/svm](../solana/svm/README.md)
- Provides `LoadedPrograms`: [solana/program-runtime](../solana/program-runtime/README.md)
