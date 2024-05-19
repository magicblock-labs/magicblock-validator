## Memory Issue

At this point the memory usage of our validator is increasing steadily. I've seen it go above
6GB. This could be due to many issues which I partially evaluated.

I'm fairly sure that this affects the performance of the validator as well as memory pressure
increases, even though this could be happening for many other reasons as the validator is
running for a long time and client apps get put into the background by the OS.

### Single Bank

We keep a single bank running which means that we don't do the same cleanup that is performed
whenever a new bank is created from a parent bank.

Thus we keep the transaction statuses around longer (and am cleaning them up roughly every 1min
to exclude that being the reason for the increased memory).

Additionally we never flush the accounts db to disk and I haven't found an obvious way to do so
without freezing the bank and creating a new one.

As a comparison the solana-test-validator under the same load (albeit it processes it much
slower) starts at about 1.32GB and may go up to 1.47GB, but then drops to 1.42GB and pretty
much stays there.

### Geyser Events Cache

In order to avoid transaction/account updates being missed when a subscription comes in late
we're caching them in the Geyser plugin. Items are evicted after a timeout.

I ruled out that this is the main reason for the memory increase as I observe it as well
without adding to that cache.

## Bench

With Geyser Cache:

- Single Run: 348MB
- Double Run: 610MB
- Triple Run: 874MB

Without Geyser Cache:

- Single Run: 285MB
- Double Run: 545MB
- Triple Run: 801MB

The mem diff seems to be a constant 65MB when using cache which means this cache memory usage
is not growing over time and thus we confirmed that the _leak_ is inside accounts db

Clearing StatusCache

- Single Run: 280MB

Not doing anything in advance slot except update ancestors and transaction processor:

- Single Run: 26.4MB -> 275MB => clearly not responsible for the larger part of the mem
increase

### Just Advancing Slot

- without even running any transactions the memory increased from 28MB to 41MB in 3mins
- without any processing in advance slot it went from 26.5MB to 26.6MB in 2mins

**Variations** (2mins)

- set_slot in `transaction_processor` -> 26.6MB to 26.7MB
- push slot to `ancestors` -> 26.3MB to 26.8MB
- no `ancestors`, `update_clock` + `fill_missing_sysvar_cache_entries` 26.8MB -> 41.8MB
- none of the above (slot, ancestors, update_clock, fill_missing_sysvar_cache_entries), but
register blockhash 26.2MB -> 26.4MB
- none of the above but `sync_loaded_programs_cache_to_slot` 26.3MB -> 26.5MB

## Identified Leaks

### Bank

```rs
transaction_log_collector.logs.push(
    TransactionLogInfo {
        signature: *tx.signature(),
        result: status.clone(),
        is_vote,
        log_messages: log_messages.clone(),
    },
);
```

- inside `load_and_execute_transactions` `sleipnir-bank/src/bank.rs:1535`, but seems smallish

### Geyser

```rs
Message::Transaction(msg) => {
    slot_messages.transactions.push(msg.transaction.clone());
    sealed_block_msg = slot_messages.try_seal();
}
```

- inside `geyser_loop` `sleipnir-geyser-plugin/src/grpc.rs:133` cloned messages to send (maybe
  channel is backed up?)
- the receiver of msgs here is unbounded, so is the blocks_meta sender and the broadcast sender
  is only bounded to 250K
- disabling entire geyser loop results in 110MB after a Single Run

## Geyser Measurements

Changed the setup to the following (halfing ITER):

```
const ITER: u64 = 50;
const THREADS: usize = 8;
const WAIT_MS: u64 = 14;
```

**Single/Second/Third Run when Geyser loop turned off:

- 26MB -> 74.7MB/112.4MB/151.2MB => about 38MB increase each run


**Single/Second Run for different broadcast channel bounds:**

- 250K: 27.1MB -> 167MB (original setting)
- 100 :  6.8MB -> 150MB / 280MB


## Adjusted Geyser Plugin to use Arcs ran 100 iterations each

With Geyser Cache turned on:

- Single Run: 209MB
- Double Run: 374MB
- Triple Run: 542MB

With Geyser Cache turned off:

- Single Run: 204MB
- Double Run: 370MB
- Triple Run: 537MB

thus an improvement in memory footprint (~168MB increase per run vs 260MB before)

After 1000 iterations with geyser cache turned on: 1.72GB


## Investigating Remaining Memory Use after Geyser Arc Change

Unless otherwise noted geyser cache was on and each run did 100 iterations

### 1. Leak 1

- `sleipnir-bank/src/bank.rs:1691`
- sizes: 32, 64, 92

```rs
transaction_log_collector.logs.push(
    TransactionLogInfo {
        signature: *tx.signature(),
        result: status.clone(),
        is_vote,
        log_messages: log_messages.clone(),
    },
);
```

Not storing transaction logs:

- Single Run: 197MB
- Double Run: 355MB
- Triple Run: 512MB

- ~158MB increase per run

### 2. Leak 2

- `sleipnir-geyser-plugin/src/plugin.rs:273` `fn notify_transaction` is cloning parts of the
`ReplicaTransactionInfoVersions` which unavoidable since we only get passed a reference
- see `sleipnir-geyser-plugin/src/grpc_messages.rs:157` clones both the sanitized transaction
and the transaction status meta

```rs
impl<'a> From<(&'a ReplicaTransactionInfoV2<'a>, u64)> for MessageTransaction {
  [..]
  Self {
      transaction: MessageTransactionInfo {
          signature: *transaction.signature,
          is_vote: transaction.is_vote,
          transaction: transaction.transaction.clone(),
          meta: transaction.transaction_status_meta.clone(),
          index: transaction.index,
      },
      slot,
  }
}
```


### 3. Leak 3

- `sleipnir-bank/src/bank.rs:1986`
- sizes: 64

```rs
// Inserts transaction status twice
status_cache.insert(
    tx.message().recent_blockhash(),
    tx.message_hash(),
    self.slot(),
    details.status.clone(),
);
status_cache.insert(
    tx.message().recent_blockhash(),
    tx.signature(),
    self.slot(),
    details.status.clone(),
);
```

Not storing by message_hash:

- Single Run: 203MB
- Double Run: 364MB
- Triple Run: 525MB

Not storing at all:

- Single Run: 199MB
- Double Run: 353MB
- Triple Run: 507MB

### 4. Leak 4

- `sleipnir-geyser-plugin/src/grpc.rs:137` `async fn geyser_loop`
- `_$LT$T$u20$as$u20$alloc..borrow..ToOwned$GT$::to_owned::h257611d42533d4cd`
- sizes: 96

### 5. Leak 5

Saw this when profiling without running the benchmark

- `sleipnir-bank/src/bank.rs:900` `fn store_account_and_update_capitalization` due to
`update_clock` when advancing slot
-> `solana/accounts-db/src/accounts_cache.rs:216` `fn store`
- sizes: 3.5KB
- removing this didn't affect the leak much


## Not subscribing for Transaction Updates with Above leaks disabled

- Single Run:  88MB
- Double Run: 130MB
- Triple Run: 171MB
- Fourth Run: 211MB

NOTE: that then it completes a bit faster as well than normal (24sec)

## Entirely Disabling Geyser + not subscribing Transactions

- Single Run: 119MB
- Double Run: 200MB
- Triple Run: 282MB
