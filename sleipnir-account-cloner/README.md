
# Summary

Implements logic for fetching remote accounts and dumping them into the local bank

Accounts come in 3 different important flavors:

- `FeePayer` accounts, which never contain data, can be used to move lamports around
- `Undelegated` accounts, which do contain data and can never be written to in the ephemeral
- `Delegated` accounts, which have a valid delegation record, therefore can be locally modified

Here are all possible cases:

- `if !properly_delegated && !has_data` -> `FeePayer`
- `if !properly_delegated && has_data` -> `Undelegated`
- `if properly_delegated && !has_data` -> `Delegated`
- `if properly_delegated && has_data` -> `Delegated`

# Logic Overview

Different types of event will trigger cloning actions:
 - A) A transaction is received in the validator
 - B) An on-chain account has changed

## Validator Transaction Received

When a transaction is received by the validator, each account of the transaction is cloned separately in parrallel.

The logic goes as follow:

- A) If the account was never seen before or changes to the account were detected since last clone
  - 0) Validate that we actually want to clone that account (is it blacklisted?)
  - 1) Start subscribing to on-chain changes for this account (so we can detect change for future clones)
  - 2) Fetch the latest on-chain account state
  - 3) Differentiate based on the account's flavor:
    - Undelegated: Simply dump the latest up-to-date fetched data to the bank
    - FeePayer: Dump the account with the lamport value found in the DelegationRecord
    - Delegated: If the account's latest delegation_slot is NOT the same as the last clone's delegation_slot, dump the latest state, otherwise ignore the change and use the cache
  - 4) Save the result of the clone to the cache
- B) If the account has already been cloned (and it has not changed on-chain since last clone)
  - 0) Do nothing, use the cache of the latest clone's result

When an on-chain account's subscription notices a change:

 - We update the `last_known_update_slot` for that account
 - On the next clone for that account, it will force the logic (A) instead of (B)
