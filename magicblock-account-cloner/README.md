
# Summary

Implements logic for fetching remote accounts and dumping them into the local bank

Accounts come in 3 different important flavors:
- `FeePayer` accounts, which never contain data, are on-curve and owned by the system program. They can be used as wallet accounts to pay fees.
- `Undelegated` accounts, which do contain data and can never be written to in the ephemeral
- `Delegated` accounts, which have a valid delegation record, therefore can be locally modified

Here are all possible cases:
- `if !properly_delegated && !has_data && is_on_curve && is_system_program_owned` -> `FeePayer`
- `if !properly_delegated && has_data` -> `Undelegated`
- `if properly_delegated && !has_data` -> `Delegated`
- `if properly_delegated && has_data` -> `Delegated`

# Logic Overview

The cloning pipeline is made out of a few components:
- The cloner (highest level) -> crate `magicblock-account-cloner`
  - The fetcher (read on-chain latest account state) -> crate `magicblock-account-fetcher`
  - The updates (subscribe to on-chain account changes) -> crate `magicblock-account-updates`
  - The dumper (apply cloned state to the bank) -> crate `magicblock-account-dumper`

## Cloning logic

Different types of event will trigger cloning actions:
- `Transaction event`: A transaction is received in the validator
- `Update event`: An on-chain account has changed

The important states stored for each account are:
- RemoteAccountUpdatesWorker.`last_known_update_slot` -> a map of which slot was the account was last updated at
- RemoteAccountUpdatesWorker.`first_subscribed_slot` -> a map of which slot was the account first subscribed at
- RemoteAccountClonerWorker.`last_clone_output` -> a cache of the latest clone's result (contains the on-chain slot at which it happened)

### Transaction event: new transaction received

When a transaction is received by the validator, each account of the transaction is cloned separately in parrallel.

Each account's clone request is pushed into a queue and executed on a worker thread dedicated to the cloner.

We can detect if an account needs to be cloned based on if the `last_known_update_slot` is more recent than the slot from which the last clone's state originated from.

For each account, the logic goes as follow:

- A) If the account was never seen before or changes to the account were detected since last clone (checks `last_known_update_slot` and compares it to the `last_clone_output`)
  - 0) Validate that we actually want to clone that account (is it blacklisted?)
  - 1) Start subscribing to on-chain changes for this account (so we can detect change for future clones)
    - This will do nothing if we already subscribed to the account before
    - This will set the `first_subscribed_slot` for that account if it's the first time we see it
  - 2) Fetch the latest on-chain account state
    - This will retry until we fetched the state of a more recent slot than `first_subscribed_slot`
    - After 5 failed retry and 200 ms sleep in between each, we fail the clone
  - 3) Differentiate based on the account's fetched flavor (we will use the "dumper"):
    - A) If Undelegated: Simply dump the latest up-to-date fetched data to the bank (programs also fetched/updated)
    - B) If FeePayer: Dump the account as-is, but with special lamport value
    - C) If Delegated: If the account's latest delegation_slot is NOT the same as the last clone's delegation_slot, dump the latest state, otherwise ignore the change and use the cache
  - 4) Save the result of the clone to the cache

- B) If the account has already been cloned (and it has not changed on-chain since last clone)
  - 0) Do nothing, use the cache of the latest clone's result into `last_clone_output`

### Update event: On-chain change detected

When an on-chain account's subscription notices a change:

- We update the `last_known_update_slot` for that account
- On the next clone for that account, it will force the logic (A) instead of (B)
- This is because the last clone's slot inside the cache will now be too old

## Update logic

During the cloner's step `A.1`, an account is added to the set of monitored acounts.
Once an account has been cloned, we keep monitoring for on-chain changes forever.

Each account's monitoring request is pushed into a queue and executed on a worker thread dedicated to the updates.
The worker maintains a list of "Shard", each shard manages a single RPC websocket connection:
- Shards are constantly created and deleted
- Each shard subscribe to EVERY monitored account at all times

On startup, we subscribe to the RPC's `Clock` changes, in order to know which slot is the latest confirmed slot for the RPC.

For each account monitoring request, an "accountSubscribe" websocket subscription is created through an RPC call.
For each account monitoring request, we set the `first_subscribed_slot` to the last `Clock`'s slot at the time of subscription.

For each update received in the websocket subscription, we save the slot at which the update occured: This is what we call the `last_known_update_slot`.

Note: multiple RPC connections are maintained at all times, and all subscriptions are refreshed every 5 minutes:
- one RPC websocket connection is destroyed (and all subscriptions dropped)
- one RPC websocket connection is created (and all subscription re-opened)

## Fetch logic

During the cloner's step `A.2`, an account fetch request is submitted to the "fetcher".
Each account's fetch request is pushed into a queue and executed on a worker thread dedicated to the fetcher.

For each fetched account, we simply use the `getMultipleAccount` solana's RPC call on both the account itself and its delegation record.
Note that we use the `minContextSlot` parameter is passed to try to enforce that the state being fetched is more recent than the latest subscription's slot.
The `minContextSlot` passed as parameter is the most recent confirmed slot which was received from the "Update" subscriptions (it's the `first_subscribed_slot`).

The fetcher structure in the validator's repository is mostly used for queuing and scheduling purposeds, most of the actual RPC request logic and parsing is done in the cunjuntoi repository implementation of the "AccountChainSnapshotProvider"
