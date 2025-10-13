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
- [ ] not yet supporting airdrop (may have to see if we only support this on a separate branch)
- [ ] remove _hack_ in svm entrypoint for magicblock program if no longer needed

## Program Deploy Issue (Fixed)

### Repro

1. run any transaction with custom program in the ER connected to local/devnet
2. ER should create an account subscription for the program
3. Redeploy the program

```
[2025-10-04T12:39:24.892039Z ERROR magicblock_chainlink::chainlink::fetch_cloner]
Failed to resolve program account 3JnJ727jWEmPVU8qfXwtH63sCNDX7nMgsLbg8qy8aaPX into bank:
The LoaderV3 program 3JnJ727jWEmPVU8qfXwtH63sCNDX7nMgsLbg8qy8aaPX needs a program data account
to be provided
```

The program is deployed on devnet

### Related

- problems cloning `PriCems5tHihc6UDXDjzjeawomAwBduWMGAi8ZUjppd` program in deployed node
- locally when using same config (pointing at helius devnet endpoint) it works fine and is
cloned via Loaderv4, including the setting of correct auth

## MaxLoadedAccountsDataSizeExceeded Issue

```
[2025-10-09T17:19:06.115843Z WARN  magicblock_aperture::requests::http]
    Failed to ensure transaction accounts:
    ClonerError(FailedToCloneRegularAccount(
        9WQsFbLPnqQ7waJqRfwSy3UMcVhzJw1HgQpiVBWnVd1k,
        TransactionError(MaxLoadedAccountsDataSizeExceeded)))
```

- none of those accounts exist on mainnet at this point
