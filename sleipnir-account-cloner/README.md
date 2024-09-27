
# Summary

Implements logic for fetching remote accounts and dumping them into the local bank

Account come in 3 different flavors:

- `Wallet` accounts, which must never contain data, can be used to move lamports around
- `Undelegated` accounts, which do contain data but can never be written to in the ephemeral
- `Delegated` accounts, which have a valid delegation record, therefore can be locally modified

# Details

For each transaction in the ephemeral we need to ensure a few things:

- 1) An account must not be able to change its flavor inside of the ephemeral (on chain is OK)
- 2) Any ephemeral transaction must ensure that it is never modifying an `Undelegated` account

# Notes

In order to achieve requirement (1) we need the following properties:

- `Delegated` accounts cannot be undelegated locally (needs re-clone from the chain), OK
- `Undelegated` accounts cannot be delegated locally (needs re-clone from the chain), OK
- `Wallet` accounts must remain wallets forever until otherwise re-cloned, NEEDS WORK
  - we must protect against a transaction allocating data on a wallet
  - TODO(vbrunet) - [HERE](https://github.com/magicblock-labs/magicblock-validator/issues/190)
