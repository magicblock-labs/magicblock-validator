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

### Recheck Fails

#### Chainlink (Fixed)

- FAIL test-chainlink::ix_feepayer ixtest_feepayer_delegated_to_us
```
thread 'ixtest_feepayer_without_ephemeral_balance' panicked at test-chainlink/tests/ix_feepayer.rs:109:5:
Expected account ErGwWjtF4vifqVwVNwMhb74RJyQXNWWt9uMWXAcf85t7 to not be cloned
```

- FAIL test-chainlink::ix_feepayer ixtest_feepayer_without_ephemeral_balance
```
thread 'ixtest_feepayer_delegated_to_us' panicked at test-chainlink/tests/ix_feepayer.rs:144:5:
Expected account 7YhoJB6ae4rB1vy1VoEk8rTPvMFp7ogw7nTcYyjmv63H to not be cloned
```


#### Cloning (Unable to reproduce)

_Flaky_ failed once and then never again
- FAIL test-cloning::01_program-deploy test_clone_mini_v4_loader_program_and_upgrade
```
Message:  assertion failed: logs.contains(&format!("Program log: LogMsg: {}",
            format!("{} upgraded", msg)))
Location: test-cloning/tests/01_program-deploy.rs:179
```

#### API (Fixed)

- FAIL test-magicblock-api::test_clocks_match test_clocks_match
- FAIL test-magicblock-api::test_get_block_timestamp_stability test_get_block_timestamp_stability
```
thread 'test_clocks_match' panicked at test-magicblock-api/tests/test_clocks_match.rs:25:10:
called `Result::unwrap()` on an `Err` value: Error { request: None, kind: RpcError(ForUser("airdrop request failed. This can happen when the rate limit is reached.")) }
```

#### Config (Fixed)

- still all failing

#### Intents

- single remaining test hangs (most likely delegation issue) (tx not confirmed)
- they cannot be fixed due to the writable issue
