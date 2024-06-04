
# Summary

Implements a `AccountsManager`, which is reponsible for:

- fetching chain accounts content
- commiting content back to chain

# Details

*Important symbols:*

- `AccountsManager` type
  - Implemented by a `ExternalAccountsManager`
  - depends on an `InternalAccountProvider` (implemented by `BankAccountProvider`)
  - depends on an `AccountCloner` (implemented by `RemoteAccountCloner`)
  - depends on an `AccountCommitter` (implemented by `RemoteAccountCommitter`)
  - depends on a `Transwise`

- `BankAccountProvider`
  - depends on a `Bank`

- `RemoteAccountCloner`
  - depends on a `Bank`

- `RemoteAccountCommitter`
  - depends on an `RpcClient`

# Notes

*Important dependencies:*

- Provides `Transwise`: the conjuncto repository
- Provides `Bank`: [sleipnir-bank](../sleipnir-bank/README.md)
