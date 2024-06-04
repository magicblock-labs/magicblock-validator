
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

*Important dependencies:*

- Provides `Bank`: [sleipnir-bank](../sleipnir-bank/README.md)
- Provides `Ledger`: [sleipnir-ledger](../sleipnir-ledger/README.md)
- Provides `AccountsManager`: [sleipnir-accounts](../sleipnir-accounts/README.md)
