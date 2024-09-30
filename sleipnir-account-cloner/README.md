
# Summary

Implements logic for fetching remote accounts and dumping them into the local bank

Accounts come in 3 different important flavors:

- `Wallet` accounts, which never contain data, can be used to move lamports around
- `Data` accounts, which do contain data and can never be written to in the ephemeral
- `Delegated` accounts, which have a valid delegation record, therefore can be locally modified

Here are all possible cases:

- `if !properly_delegated && !has_data` -> `Wallet`
- `if !properly_delegated && has_data` -> `Undelegated`
- `if properly_delegated && !has_data` -> `Delegated`
- `if properly_delegated && has_data` -> `Delegated`

# Details

For each transaction in the ephemeral we need to ensure a few things:

- 1) An account must not be able to change its flavor inside of the ephemeral (on chain change is OK)
- 2) Any ephemeral transaction must ensure that it is never modifying any `Data` account
- 3) Any `Wallet` lamports must have been escrowed in and out on the base chain

Assuming the above requirements are fullfilled, this means transactions in the ephemeral can:

- Send lamports freely from/to `Wallet` accounts and `Delegated` accounts
- `Wallet` can be used as payer for both transaction fee and rent
- Modify state of `Delegated` accounts
- Use `Data` accounts as read-only

# Notes

`Wallet` account must not contain data, since their lamports balance is escrowed, it will not be an exact mirror of the base chain's lamport balance. Therefore the account must not need to pay rent in order to be able to exist in the ephemeral since its escrowed lamport value has no guarantee to cover rent.

In order to achieve requirement (1) we need the following properties:

- `Delegated` accounts cannot be undelegated locally (needs re-clone from the chain), OK
- `Data` accounts cannot be delegated locally (needs re-clone from the chain), OK
- `Wallet` accounts must remain wallets forever until otherwise re-cloned, NEEDS WORK
  - we must protect against a transaction allocating data on a wallet
  - TODO(vbrunet) - [HERE](https://github.com/magicblock-labs/magicblock-validator/issues/190)
