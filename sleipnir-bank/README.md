
# Summary

Complete solana's SVM client, capable of running transactions

# Details

*Important symbols:*

- `Bank` struct
  - Basically contains a full SVM chain state
  - Implements a bunch of traits from solana's SVM
  - It's basically a fully fledged solana client with all utils (Fees/Logs/Slots/Rent/Cost)
  - Contains a `BankRc`
  - Contains a `StatusCache`
  - Uses `TransactionBatchProcessor` for simulating and executing transactions
  - Shares a `LoadedPrograms` with the transaction processor

- `BankRc` struct
  - Contains an `Accounts`

- `StatusCache` struct
  - It's basically a `HashMap<Hash, (Slot, HashMap<Key, Vec<(Slot, T)>>)>`
  - // TODO(vbrunet) - figure out exactly how data structure works

# Notes

*Important dependencies:*

- Provides `Accounts`: [solana/accounts-db](../solana/accounts-db/README.md)
- Provides `TransactionBatchProcessor`: [solana/svm](../solana/svm/README.md)
- Provides `LoadedPrograms`: [solana/program-runtime](../solana/program-runtime/README.md)
