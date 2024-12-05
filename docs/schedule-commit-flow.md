## Schedule Commit Flow

### 1. User program issues a `schedule_commit` instruction via CPI as part of a transaction.

- at this point it is verified that the program owning the PDA(s) to be committed is the direct
  caller of the CPI
- the `schedule_commit` instruction cannot be called directly or from any other program but the
  one owning the PDA(s) to be committed, otherwise the transaction will fail

### 2. Processing the `ScheduleCommit | ScheduleCommitAndUndelegate` instruction

- as a result of processing the `schedule_commit` instruction, a `ScheduledCommit` is created
- it includes the following:
  - `slot` at which the commit is scheduled
  - `blockhash` at the slot
  - `accounts` to be committed
  - `payer` pubkey
  - `owner` pubkey of the program owning the PDA(s) to be committed
  - `commit_sent_transaction` (see below)
  - `request_undelegation` flag indicating whether the commit is requesting undelegation
- if undelegation is requested for an account then its owner is changed to the delegation
program in the ephemeral to practically disable it to be used as _writable_ given that the
schedule commit transaction succeeds
- the `ScheduledCommit` is added to the `MagicContext1111...` account
- it is not directly added to a global to make it transactional, i.e. if the transaction
containing the `schedule_commit` instruction fails then the `ScheduledCommit` is not added to
the account either

#### The Commit Sent Transaction

- the `commit_sent_transaction` is created assuming it is processed while the current blockhash
  is still valid, thus its signature can be logged right here so the client can follow the
chain of events
- the signature of this transaction can be pre-calculated since we pass the ID of the commit
(to chain) and retrieve the signature from a globally stored hashmap once the transaction
executes
- the chain transaction signature couldn't be known since it depends on the instruction args
which in turn depend on the state of the accounts we are committing at the time we process it


### 3. Accepting scheduled commits via the `AcceptScheduleCommits` instruction

- deserializes the `MagicContext1111...` account and _moves_ all scheduled commits from this
account over to a global
- the `MagicContext1111...` account scheduled commits `Vec` is cleared
- after this is completed the `TransactionScheduler` holds the state of those scheduled commits

```rs
#[derive(Clone)]
pub struct TransactionScheduler {
    scheduled_commits: Arc<RwLock<Vec<ScheduledCommit>>>,
}
```

### 4. Sending Scheduled Commits to Chain

- the scheduled commits are processed - sent to chain via the `ScheduledCommitsProcessor`
implemented by the `RemoteScheduledCommitsProcessor`
- it takes all scheduled commits from the `TransactionScheduler` and sends them to chain
- the previously prepared `ScheduledCommitSent` transaction processed
- as part of it the `register_scheduled_commit_sent` which stored the `SentCommit` information
  under the `ScheduledCommit` ID such that we can print the chain transaction signature
- after the ephemeral _event_ transaction runs we processs the commits in the background,
ensuring that the transaction is confirmed, however only an error is printed to the log should
it fail

#### Ledger Replay

- when we replay the ledger this the `register_scheduled_commit_sent` is never performed and
thus the `SentCommit` information is not stored
- however the `SentCommit` transaction still executes and we detect this by checking a
_validator started_ flag in order to bypass the logic that is not useful during ledger replay
- since the `RemoteScheduledCommitsProcessor` is not running during ledger replay no scheduled
  commits are processed

NOTE: we should verify the state of replayed commits that never were realized, namely the
following:

- state of the `MagicContext1111...` account
- state of globally `ScheduledCommits`

Most likely we need to clear both of them before we start the validator after ledger replay.

At least we need to clear the `ScheduledCommits` - the `MagicContext1111...` account is
cleared after the scheduled commits are moved to the global.
