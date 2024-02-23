## Sysvars

[docs: `docs/src/runtime/sysvars.md`](https://docs.solanalabs.com/runtime/sysvars)

Sysvars are vars holding validator related information that are either accessible via a
specific sysvar account provided to the transaction or via `Sysvar::get` (in most cases).

The following sysvars exist and are supported by our validator if they have a `*`:

- `clock`*: current slot, epoch, and leader schedule
- `epoch_schedule`*: epoch schedule (does not change during the life of a blockchain)
- `fees`*: fees charged for processing a transaction (deprecated, but we support it for now)
- `recent_blockhashes`*: recent blockhashes (supported but `RecentBlockhashes::get` is not availble - nor is it on Solana)
- `rent`*: rent parameters
- `slot_hashes`: recent slot hashes (Solana updates this only when new bank is created from a parent)
- `slot_history`: recent slot history (Solana updates this only when bank is frozen)
- `stake_history`: recent stake history (makes no sense in our case which means we only will stub it or not support ever)
- `epoch_rewards`: progress of epoch rewards distribution (also makes no sense in our case, Solana creates this when calculating/distributing rewards)
- `last_restart_slot`*: last restart slot (set to `0`, but currently not enabled with the feature set we use )
