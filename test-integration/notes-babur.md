## Integration Test Status

- [x] `schedulecommit/test-scenarios`
- [x] `schedulecommit/test-security`
- [x] `test-chainlink`
- [x] `test-cloning`
- [x] `test-committor-service`
- [x] `test-issues` removed since we won't support frequent commits
- [x] `test-table-mania` all passing
- [ ] `test-config` all failing
- [ ] `test-ledger-restore` all failing/stalling
- [ ] `test-magicblock-api` 2/4 failing (incorrect airdrop)
- [ ] `test-pubsub` failing due to airdrop similar to above
- [ ] `test-schedule-intent` all failing (not sure why)

### Test Config

When running individually I get:

```sh
cargo test test_clone_config_never -- --nocapture
```

```
called `Result::unwrap()` on an `Err` value: LedgerError(UnableToSetOpenFileDescriptorLimit)
```
when the validator starts up.

### Test Ledger Restore

```sh
make setup-restore-ledger-devnet
````

Then run test: ` cargo nextest run restore_ledger_empty_validator --nocapture`

Error:
```
Failed to start validator: NextSlotAfterLedgerProcessingNotMatchingBankSlot(19, 20)
```

### Test Magicblock API

```sh
make setup-magicblock-api-devnet
```

```sh
make setup-magicblock-api-ephem
```

Then run test: ` cargo nextest run -p test-magicblock-api  --no-fail-fast -j 16`

Errors:

```
FAIL [   0.126s] test-magicblock-api::test_clocks_match test_clocks_match
FAIL [   0.127s] test-magicblock-api::test_get_block_timestamp_stability test_get_block_timestamp_stability
```
- failing due to airdrop disabled
- should use integration test context and call `pub fn airdrop_chain_escrowed`

## UX

- need to have transaction in ledger if writable vs. delegated doesn't check out
- other option is to have _fake_ hashmap of signatures that provide this error

## Perf

- we run a tx to look into the magic context for scheduled commits very frequently
- we should peek into the account instead and only run the tx if it changed (has scheduled
commits)

## ~~Regression~~ _Fixed_

- [fix here](https://github.com/magicblock-labs/magicblock-validator/commit/9ad32f3be6a13984f1a7ff897f1b6b462cbc7395)

### Schedule Commit

- `test_committing_and_undelegating_two_accounts_modifying_them_after` fails because the
scheduled commit appears to be still is processed even though the transaction scheduling it fails
- however looking at the committed account keys (see below) it is actually a different commit,
  just that the signature matches
- this could be confusing for users as they think their commit has been processed when it
  actually failed

#### Repro

1. Terminal 1: `make programs && make setup-schedulecommit-devnet`
2. Terminal 2: `make setup-schedulecommit-ephem`

Third terminal run either of the below:

###### Not reproducing issue

```sh
cargo nextest run test_committing_and_undelegating_two_accounts_modifying_them_after --nocapture
```

This passes usually (indicating the problem is due to a race condition).

###### Reproducing issue

```sh
cargo nextest run -p schedulecommit-test-scenarios --no-fail-fast -j 16
```

This fails pretty consistently.

I was able to repro this by disabling all tests (commenting `// #[test]`) except
- test_committing_and_undelegating_two_accounts_success
- test_committing_and_undelegating_two_accounts_modifying_them_after

#### Problem

When it fails we see the following for the (correctly) failed transaction:

```
Signature: L4kRWjjMxzRp3W4XUbAWdrr4HgQ6Q4XkmepPf7ZHjkNi5Fo1gwyTLdz5hk76iREdVeBHoNiEnAgViqjeES2UdFi
> Program logged: "Processing schedulecommit_and_undelegation_cpi_with_mod_after instruction"
> Program invoked: Unknown Program (Magic11111111111111111111111111111111111111)
  > ScheduleCommit: parent program id: 9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY
  > ScheduleCommit: account BtQ2ZUQrFKJKuHEgFbrey9cr4eWLtDfeCHUtByAEDvKn owner set to delegation program
  > ScheduleCommit: account ENDmT3tWsvURsKru1EWM3ixAHE3zv7tnR8Sx2gKxdvWJ owner set to delegation program
  > Scheduled commit with ID: 563
  > ScheduledCommitSent signature: 5HoxcgESqyLiCz8YPw8bHvt3xkMniKQ6Cp5wLyW4NgkXMzAk3ZHBf6aSJoyaHyfNUTdEm1QmDnmaKDPMmXzfz5qj
  > Program returned success
> Program consumed: 25866 of 200000 compute units
> Program returned error: "instruction modified data of an account it does not own"
```
_It tries to modify the counter accounts after it commit + undelegation was requested in same
transaction._

However we still see that commit getting processed (it was still persisted to the magic context
account even though the transaction failed).

Tricky part is that it is actually a different commit, just that it is done using the same
transaction signature.

```
Signature: 5HoxcgESqyLiCz8YPw8bHvt3xkMniKQ6Cp5wLyW4NgkXMzAk3ZHBf6aSJoyaHyfNUTdEm1QmDnmaKDPMmXzfz5qj
Unknown Program Instruction
> ScheduledCommitSent id: 563, slot: 15528, blockhash: 8dgSRHzm9NRYa98F4QSwGUaCEsULTuA82AU4BRZj5tpy
> ScheduledCommitSent payer: 7YFFvXC8ymPwjF6wU5hRrMUz9ZMCPpxbGWqW26ApssVV
> ScheduledCommitSent included: [EbmGz99yveTEtQyei51ZM1qiLjtjNZ72YbPFiv8bXVzV, 2spAy4qcZvoP48PKSjCnQtQJmomrYKtTsXnSTJyYoNSd]
> ScheduledCommitSent excluded: []
> ScheduledCommitSent signature[0]: 44iCY6TaV23KpDyc2Bxb1FBpRXjZAnhzMhMxpS1w7dNyXJmZqsg8LKc8mtGwyLTfTc5927Zrh5mvu17ETgV4CwUn
> ScheduledCommitSent requested undelegation
```
