## TPU

[docs](https://docs.rs/solana-core/latest/solana_core/tpu/index.html)

- implements the Transaction Processing Unit, a multi-stage transaction processing pipeline in software

### Pieces

- `FetchStage` to fetch transactions from the network via UDP
  - `TpuSockets` to receive packets representing transactions
- `StakedNodesUpdaterService`
- `SigVerifyStage` to verify the signatures of the transactions on GPU
  - `TransactionSigVerifier` which takes votes into account
  - `ClusterInfoVoteListener` also for voting related parts
- `BankingStage` to process the transactions and update the bank state
- `BroadcastStage` to broadcast the processed transactions to the network
  - quic_streamer_tpu and quic_streamer_tpu_forwards


## Banking Stage

[docs](https://docs.rs/solana-core/latest/solana_core/banking_stage/index.html)

> Stores the stage’s thread handle and output receiver.

- creates `num_threads` derived from `MIN_TOTAL_THREADS = 3` or from `SOLANA_BANKING_THREADS` env var
  - [issue related to what is a good number for this var](https://github.com/solana-labs/solana/issues/24163) (4-8) with 4 giving 1.5K txs/sec
  - prior to 1.9 higher worked better, but that issue may have been fixed now which means we
  could use a value more simular to 0.8 * CPU_CORES or similar `num_cpus::get() * 0.8` provided
  we don't have a bunch of other (rayon) threads
  > Initially, it was extremelly high 24. We have a lot of cpu cores on our server so we can afford such value. On v1.8 it worked without any problem, I monitored our validator from time to time and saw that it produced decent blocks with up to 4.4k transactions per block.

### Included Pieces we don't Need for now

**BlockProduction**:

```
block_production_method: BlockProductionMethod,
```

**ClusterInfo**:

```
cluster_info: &Arc<ClusterInfo>,
```

**Poh**:

```
poh_recorder: &Arc<RwLock<PohRecorder>>,
```

**Voting**:

```
non_vote_receiver: BankingPacketReceiver,
tpu_vote_receiver: BankingPacketReceiver,
gossip_vote_receiver: BankingPacketReceiver,
```

- uses different scheduler strategies `new_central_scheduler` and `new_thread_local_multi_iterator`

### new_central_scheduler

- [Central Scheduling Thread](https://apfitzge.github.io/posts/solana-scheduler/#central-scheduling-thread)
- determines ahead of time how to partition transactions onto threads and thus avoids txs
having to be skipped an pushed onto the next iteration
- scheduler is the only thread which accesses the channel from `SigVerify`
- scheduler maintains a view of which account locks are in-use by which threads, and is able to
determine which threads a transaction can be queued on
- `DecisionMaker` decides what to do with each transaction
- `Committer` commits executed transactions to the bank
- for some reason still spawns legacy voting threads (we won't need those)
- sets up workers and `finished_work` channel for each worker
- each worker creates a `ConsumeWorker` with a `Consumer` which includes a clone of the `Committer`
- spawns a thread for each worker (`ConsumerWorker::run`)
- run `loop`s pulling off work via `let work: ConsumerWork = self.consume_receiver.recv()?;`
- consumes batches via `consumer.process_and_record_aged_transactions`



- more details in ./central-scheduler.md`


### new_thread_local_multi_iterator (default, but deprecated)

- Single thread to generate entries from many banks.
- This thread talks to poh_service and broadcasts the entries once they have been recorded.
- Once an entry has been recorded, its blockhash is registered with the bank.
- uses _Many banks that process transactions in parallel_
- handles the following via different receivers and storage:
  - gossip_vote
  - tpu_vote
  - non_vote
- includes a _forwarder_ for poh broadcast that we don't need
- invokes `spawn_thread_local_multi_iterator_thread`

- for non_vote creates the below transaction storage

```rust
UnprocessedTransactionStorage::new_transaction_storage(
    UnprocessedPacketBatches::with_capacity(batch_limit),
    ThreadType::Transactions,
),
```

### spawn_thread_local_multi_iterator_thread

- creates packet_receiver from [`BankingPacketReceiver`](https://docs.rs/solana-core/latest/solana_core/banking_trace/type.BankingPacketReceiver.html)
- creates
[`Consumer`](https://docs.rs/solana-core/latest/solana_core/banking_stage/consumer/struct.Consumer.html)
which can process transactions via a provided `BankStart` which includes the `working_bank`
- spawns a thread which runs a `process_loop` with the `Consumer` and `packet_receiver`

### process_loop

1. checks [`UnprocessedTransactionStorage`](https://docs.rs/solana-core/latest/solana_core/banking_stage/unprocessed_transaction_storage/enum.UnprocessedTransactionStorage.html) for transactions to process
2. if non-empty invokes `process_buffered_packets` with them
3. `packet_receiver.receive_and_buffer_packets` to update the `UnprocessedTransactionStorage`
  - `unprocessed_transaction_storage.insert_batch(deserialized_packets)`

```rust
// core/src/banking_stage/immutable_deserialized_packet.rs :37
#[derive(Debug, PartialEq, Eq)]
pub struct ImmutableDeserializedPacket {
    original_packet: Packet,
    transaction: SanitizedVersionedTransaction,
    message_hash: Hash,
    is_simple_vote: bool,
    compute_budget_details: ComputeBudgetDetails,
}
```

### Banking Stage Resources

- [Solana Banking Stage and Scheduler](https://apfitzge.github.io/posts/solana-scheduler/) Nov 2023
- [What’s new with Solana’s transaction scheduler?](https://medium.com/@harshpatel_36138/whats-new-with-solana-s-transaction-scheduler-bcf79a7d33f7) Jan 22, 2024
