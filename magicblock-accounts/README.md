
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
  - Implements `ensure_accounts` function
  - Maintains a local cache of accounts already validated and cloned

- `BankAccountProvider`
  - depends on a `Bank`

- `RemoteAccountCloner`
  - depends on a `Bank`

- `RemoteAccountCommitter`
  - depends on an `RpcClient`

# Notes

*How does `ensure_accounts` work:*

- Collect readonly and writable accounts that we haven't already cloned in the validator
- Those accounts we haven't seen yet we "validate" using `Transwise.validate_accounts`
- We need all accounts to be cloned for the transaction to run, so we clone accounts after the validation
- We also set the correct owners on cloned delegated accounts so that the smart contracts can use them
- Also fund the payer lamports so that it can pay for transactions costs
- Also modify the delegated accounts to have the original owner inside the validator

*Important dependencies:*

- Provides `Transwise`: the conjuncto repository
- Provides `Bank`: [magicblock-bank](../magicblock-bank/README.md)
