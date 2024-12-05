
# Summary

Implements a RPC server using `jsonrpc` library.
This RPC has the same API as the solana RPC.
However any transaction sent to this RPC is ran inside the custom SVM bank.

# Details

*Important symbols:*

- `JsonRpcService` struct
  - depends on a `JsonRpcRequestProcessor`
  - Registers the method handlers:
    - `FullImpl` (send_transaction, simulate_transaction, and important ones)
    - `AccountsDataImpl` (get_account_info, etc)
    - `AccountsScanImpl` (get_program_accounts, get_supply)
    - `BankDataImpl` (get_slot_leader, get_epoch_schedule, etc)
    - `MinimalImpl` (get_balance, get_slot, etc)

- `JsonRpcRequestProcessor` struct
  - depends on a `Bank`
  - depends on a `Ledger`
  - depends on an `AccountsManager`

- `FullImpl` struct
  - Contains implementations for important RPC methods
  - Uses `JsonRpcRequestProcessor` under the hood for most logic

# Notes

*How are `send_transaction` requests handled:*

- `decode_and_deserialize` deserialize a `String` into a `VersionedTransaction`
- `SanitizedTransaction::try_create` with the `Bank`
- `sig_verify_transaction` is ran, which uses `SanitizedTransaction.verify`
- `AccountsManager.ensure_accounts` is ran
- `transaction_preflight` (uses `Bank.simulate_transaction_unchecked`)
- `Bank.prepare_sanitized_batch`
- `execute_batch` which uses `Bank.load_execute_and_commit_transactions`

*Important dependencies:*

- Provides `Bank`: [magicblock-bank](../magicblock-bank/README.md)
- Provides `Ledger`: [magicblock-ledger](../magicblock-ledger/README.md)
- Provides `AccountsManager`: [magicblock-accounts](../magicblock-accounts/README.md)
- Provides `execute_batch`: [magicblock-processor](../magicblock-processor/README.md)
