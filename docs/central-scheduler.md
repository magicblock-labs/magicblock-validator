## Central Scheduler

- fed into by after passing `DecisionMaker` filter which provides
```rust
pub enum BufferedPacketsDecision {
    Consume(BankStart),
    Forward,
    ForwardAndHold,
    Hold,
}
```
- in our case there is no reason to `Forward`
- `Hold` is used when there is no active bank (also unlikely in our case)

### SchedulerController

1. receives transacions via packets `packet_receiver: PacketDeserializer` decides what to do
with them `decision_maker: DecisionMaker`
2. schedules them via `scheduler: PrioGraphScheduler`


**SchedulerController::run** loop:

1. decision
2. process_transactions based on _decision_ .. for us it should be always `Consume`
2.1. schedules transactions (some maybe unschedulable and are pushed back into `TransactionStateContainer`)

**`schedule`**:
  - takes two predicates
  - `pre_graph_filter: impl Fn(&[&SanitizedTransaction], &mut [bool]),`
  - `pre_lock_filter: impl Fn(&SanitizedTransaction) -> bool,` (`true` for all cases _for now_)

> Schedule transactions from the given `TransactionStateContainer` to be
> consumed by the worker threads. Returns summary of scheduling, or an
> error.
> `pre_graph_filter` is used to filter out transactions that should be
> skipped and dropped before insertion to the prio-graph. This fn should
> set `false` for transactions that should be dropped, and `true`
> otherwise.
> `pre_lock_filter` is used to filter out transactions after they have
> made it to the top of the prio-graph, and immediately before locks are
> checked and taken. This fn should return `true` for transactions that
> should be scheduled, and `false` otherwise.
>
> Uses a `PrioGraph` to perform look-ahead during the scheduling of transactions.
> This, combined with internal tracking of threads' in-flight transactions, allows
> for load-balancing while prioritizing scheduling transactions onto threads that will
> not cause conflicts in the near future.

- the accounts of the not dropped txs are locked `Self::get_transaction_account_access`
- then keeps `pop`ing from the `PrioGraph` and schedules transaction if possible onto the
appropriate thread
- adds to `Batches` `transactions` for the particular thread
- other txs are pushed back into `TransactionStateContainer`

```rust
struct Batches {
    ids: Vec<Vec<TransactionId>>,
    transactions: Vec<Vec<SanitizedTransaction>>, // transaction batch by thread_id
    max_age_slots: Vec<Vec<Slot>>,
    total_cus: Vec<u64>,
}
```

- the batch is then sent via `self.consume_work_senders[thread_index].send(work)`

