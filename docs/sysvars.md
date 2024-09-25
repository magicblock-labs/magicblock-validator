## Sysvars

[docs: `docs/src/runtime/sysvars.md`](https://docs.solanalabs.com/runtime/sysvars)

Sysvars are vars holding validator related information that are either accessible via a
specific sysvar account provided to the transaction or via `Sysvar::get` (in most cases).

The following sysvars exist and are supported by our validator if they have a `*`:

- `clock`*: current slot, epoch, and leader schedule
- `epoch_schedule`*: epoch schedule (does not change during the life of a blockchain)
- `fees`*: fees charged for processing a transaction (deprecated, but we support it for now)
  - currently we provide `lamports_per_signature: 0` though which isn't correct
- `recent_blockhashes`*: recent blockhashes (supported but `RecentBlockhashes::get` is not available - nor is it on Solana)
  - here we provide the recent blockhash also with incorrect `lamports_per_signature = 0`
- `rent`*: rent parameters
- `slot_hashes`: recent slot hashes (Solana updates this only when new bank is created from a parent)
- `slot_history`: recent slot history (Solana updates this only when bank is frozen)
  > [A bitvector indicating which slots are present in the past epoch](https://docs.rs/solana-sdk/latest/solana_sdk/sysvar/slot_history/struct.SlotHistory.html)
  > Holds an array of slots available during the most recent epoch in Solana, and it is updated every time a new slot is processed.
- `stake_history`: recent stake history (makes no sense in our case which means we only will stub it or not support ever)
- `epoch_rewards`: progress of epoch rewards distribution (also makes no sense in our case, Solana creates this when calculating/distributing rewards)
- `last_restart_slot`*: last restart slot (set to `0`, but currently not enabled with the feature set we use )

The program to test sysvars is defined inside `test-integration/sysvars`.
The related tests are defined inside `sleipnir-bank/tests/transaction_execute.rs` as
`test_bank_sysvars_get` and `test_bank_sysvars_from_account`.

### Resources

- [solana_sdk/sysvar](https://docs.rs/solana-sdk/latest/solana_sdk/sysvar/index.html)
- [rareskills.io/post/solana-sysvar](https://www.rareskills.io/post/solana-sysvar)
