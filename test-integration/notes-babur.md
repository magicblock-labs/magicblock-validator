## Integration Test Status

- [x] `schedulecommit/test-scenarios`
- [x] `schedulecommit/test-security`
- [x] `test-chainlink`
- [x] `test-cloning`
- [x] `test-committor-service`
- [x] `test-issues` removed since we won't support frequent commits
- [x] `test-table-mania` all passing
- [x] `test-config` 2/2 failing (Transaction::sign failed with error NotEnoughSigners -fixed) (Thorsten)
- [x] `test-ledger-restore`  all passing except one test no longer supported
`11_undelegate_before_restart`
- [x] `test-magicblock-api` all passing now
- [x] `test-pubsub` were failing due to airdrop similar to above and were fixed via escrowed airdrop
- [x] `test-schedule-intent` 5/5 failing (failed to airdrop) (Babur - disabled)
- [x] clone not found escrow accounts with 0 lamports (Thorsten - fixed)
- [x] replay - remove all non-delegated accounts from bank (Thorsten - fixed)
- [x] correctly handle empty readonly accounts (Thorsten)
- [x] we are removing programs on resume, ensured ledger replay completed before that (Thorsten)
- [x] magicblock-aperture/src/requests/http/get_fee_for_message.rs should check blockhash (Babur)
- [x] `self.blocks.contains(hash)` times out - noticed while investigating issue (Babur)
    - + why aren't we using that instead of `self.blocks.get(hash)`?
- [x] we won't know if an account delegated to system program is updated or undelegated, but I
  suppose that is ok since we treat them as isolated in our validator? (Gabriele)
  - commits of those would fail (Gabriele) and the committor won't retry (Edwin)
- [x] LRU cache capacity from config set to const for now (https://github.com/magicblock-labs/magicblock-validator/issues/577)
- [x] ignore sub update that's out of order
- [x] fix all use of `_` when assigning tmp dir in tests

## TODOs

- [ ] not yet supporting airdrop (may have to see if we only support this on a separate branch)
- [x] remove _hack_ in svm entrypoint for magicblock program if no longer needed (improved via
  separate ixs)

## After Master Merge 2

- [x] test-chainlink/tests/ix_full_scenarios.rs failing
- [x] task scheduler tests failing (Program cloning issue)

## After Master Merge 1

- [x] test-schedulecommit
- [x] test-chainlink
- [x] test-cloning
- [x] test-restore-ledger
- [x] test-magicblock-api
- [x] test-table-mania
- [x] test-committor
- [x] test-pubsub
- [x] test-config
- [x] test-schedule-intents
- [x] test-task-scheduler (fixed by Dode)

## Unit Test Status

### Need Babur's Help _Fixed_

Not sure why these fail (assume `0` return value)

- magicblock-aperture::mocked test_get_epoch_schedule
- magicblock-aperture::mocked test_get_supply - not sure why this fails (Babur)

#### Failing with `RpcError(DeadlineExceeded)`

This is due to `InvalidFeePayerForTransaction`, we need to delegate the account.
However that fails since we need to _add_ an `Account` to the test env which looses the
_delegated_ flag.

Either we add that flag to the `Account` struct or we need to modify the account via a
transaction in the test (not sure if that is possible).

See [this slack thread](https://magicblock-labs.slack.com/archives/C07QF4P5HJ8/p1760608866099959).

- magicblock-committor-program::prog_init_write_and_close test_init_write_and_close_extremely_large_changeset
- magicblock-committor-program::prog_init_write_and_close test_init_write_and_close_insanely_large_changeset
- magicblock-committor-program::prog_init_write_and_close test_init_write_and_close_large_changeset
- magicblock-committor-program::prog_init_write_and_close test_init_write_and_close_small_changeset
- magicblock-committor-program::prog_init_write_and_close test_init_write_and_close_small_single_account
- magicblock-committor-program::prog_init_write_and_close test_init_write_and_close_very_large_changeset

### Need Edwin's Help

Tests inside `programs/magicblock/src/schedule_transactions/process_schedule_commit_tests.rs`
are failing on an `assert` that was added with intents in CI only.

## Test Node _Fixed_

Problems below most likely caused due to restarting with an incompatible accountsdb snapshot.
We may need a migration script to be able to restart from an older snapshot.

### Program Deploy _Fixed_

- problems cloning `PriCems5tHihc6UDXDjzjeawomAwBduWMGAi8ZUjppd` program in deployed node
- locally when using same config (pointing at helius devnet endpoint) it works fine and is
cloned via Loaderv4, including the setting of correct auth

### MaxLoadedAccountsDataSizeExceeded Issue

```
[2025-10-09T17:19:06.115843Z WARN  magicblock_aperture::requests::http]
    Failed to ensure transaction accounts:
    ClonerError(FailedToCloneRegularAccount(
        9WQsFbLPnqQ7waJqRfwSy3UMcVhzJw1HgQpiVBWnVd1k,
        TransactionError(MaxLoadedAccountsDataSizeExceeded)))
```

- none of those accounts exist on mainnet at this point
