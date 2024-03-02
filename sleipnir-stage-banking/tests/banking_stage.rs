use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use crossbeam_channel::unbounded;
use log::{debug, error, info};
use serde::Serialize;
use sleipnir_bank::{
    bank::Bank,
    bank_dev_utils::{
        init_logger,
        transactions::{create_funded_accounts, execute_transactions},
    },
    genesis_utils::{create_genesis_config, GenesisConfigInfo},
};
use sleipnir_stage_banking::{
    banking_stage::BankingStage, packet::BankingPacketBatch,
    transport::banking_tracer::BankingTracer,
};
use sleipnir_transaction_status::{
    TransactionStatusBatch, TransactionStatusMessage, TransactionStatusSender,
};
use solana_measure::{measure::Measure, measure_us};
use solana_perf::packet::{to_packet_batches, PacketBatch};
use solana_sdk::{native_token::LAMPORTS_PER_SOL, system_transaction};

const LOG_MSGS_BYTE_LIMT: Option<usize> = None;

fn convert_from_old_verified(mut with_vers: Vec<(PacketBatch, Vec<u8>)>) -> Vec<PacketBatch> {
    with_vers.iter_mut().for_each(|(b, v)| {
        b.iter_mut()
            .zip(v)
            .for_each(|(p, f)| p.meta_mut().set_discard(*f == 0))
    });
    with_vers.into_iter().map(|(b, _)| b).collect()
}

fn watch_transaction_status(
    tx_received_counter: Arc<AtomicU64>,
) -> (Option<TransactionStatusSender>, std::thread::JoinHandle<()>) {
    let (transaction_status_sender, transaction_status_receiver) = unbounded();
    let transaction_status_sender = Some(TransactionStatusSender {
        sender: transaction_status_sender,
    });
    let tx_status_thread = std::thread::spawn(move || {
        let transaction_status_receiver = transaction_status_receiver;
        while let Ok(TransactionStatusMessage::Batch(batch)) = transaction_status_receiver.recv() {
            debug!("received batch: {:#?}", batch);
            tx_received_counter.fetch_add(batch.transactions.len() as u64, Ordering::Relaxed);
        }
    });
    (transaction_status_sender, tx_status_thread)
}

#[test]
fn test_banking_stage_shutdown1() {
    init_logger();

    let genesis_config_info = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config_info.genesis_config);
    let bank = Arc::new(bank);

    let banking_tracer = BankingTracer::new_disabled();
    let (non_vote_sender, non_vote_receiver) = banking_tracer.create_channel_non_vote();

    let banking_stage = BankingStage::new(non_vote_receiver, None, LOG_MSGS_BYTE_LIMT, bank, None);
    drop(non_vote_sender);
    banking_stage.join().unwrap();
}

#[test]
fn test_banking_stage_with_transaction_status_sender_perf() {
    init_logger();
    solana_logger::setup();

    const SEND_CHUNK_SIZE: usize = 100;

    let num_transactions: Vec<u64> = vec![1, 2, 5, 10, 100, 1000]; //, 10_000];
    let batch_sizes: Vec<u64> = vec![1, 2, 4, 8, 16, 32, 64, 128];
    let thread_counts: Vec<u64> = vec![1, 2, 3, 4, 5, 6];

    let permutations = num_transactions
        .iter()
        .flat_map(|num_transactions| {
            batch_sizes.iter().flat_map(|batch_size| {
                thread_counts
                    .iter()
                    .map(|thread_count| (*num_transactions, *batch_size, *thread_count))
            })
        })
        .collect::<Vec<_>>();

    let mut results: Vec<BenchmarkTransactionsResult> = Vec::with_capacity(permutations.len());
    for (num_transactions, batch_size, thread_count) in permutations {
        let num_payers: u64 = (num_transactions / thread_count).min(thread_count).max(1);
        let config = BenchmarkTransactionsConfig {
            num_transactions,
            num_payers,
            send_chunk_size: SEND_CHUNK_SIZE,
            batch_chunk_size: batch_size as usize,
        };
        let result = run_bench_transactions(config);
        results.push(result);
    }
    let mut wtr = csv::Writer::from_path("/tmp/out.csv").expect("Failed to create CSV writer");
    for result in &results {
        wtr.serialize(result).expect("Failed to serialize");
    }
    wtr.flush().expect("Failed to flush");
}

#[derive(Debug)]
struct BenchmarkTransactionsConfig {
    pub num_transactions: u64,
    pub num_payers: u64,
    pub send_chunk_size: usize,
    pub batch_chunk_size: usize,
}

#[derive(Debug, Serialize)]
struct BenchmarkTransactionsResult {
    pub num_transactions: u64,
    pub num_payers: u64,
    pub send_chunk_size: usize,
    pub batch_chunk_size: usize,
    pub execute_batches_and_receive_results_elapsed_ms: u64,
}

fn run_bench_transactions(config: BenchmarkTransactionsConfig) -> BenchmarkTransactionsResult {
    info!("{:#?}", config);
    let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);
    let start_hash = bank.last_blockhash();
    let bank = Arc::new(bank);

    let banking_tracer = BankingTracer::new_disabled();
    let (non_vote_sender, non_vote_receiver) = banking_tracer.create_channel_non_vote();

    // 1. Fund an account so we can send 2 good transactions in a single batch.
    debug!("1. funding payers...");
    let payers = create_funded_accounts(
        &bank,
        config.num_payers as usize,
        Some(LAMPORTS_PER_SOL * (config.num_transactions / config.num_payers)),
    );

    // 2. Create the banking stage
    debug!("2. creating banking stage...");

    let tx_received_counter = Arc::<AtomicU64>::default();
    let (transaction_status_sender, tx_status_thread) =
        watch_transaction_status(tx_received_counter.clone());
    let banking_stage = BankingStage::new(
        non_vote_receiver,
        transaction_status_sender,
        LOG_MSGS_BYTE_LIMT,
        bank,
        Some(config.batch_chunk_size),
    );

    // 3. Create Transactions
    debug!("3. creating transactions...");
    let (_accs, txs) = (0..config.num_transactions)
        .map(|idx| {
            let payer = &payers[(idx % config.num_payers) as usize];
            let to = solana_sdk::pubkey::Pubkey::new_unique();
            (
                to,
                system_transaction::transfer(payer, &to, 890_880_000, start_hash),
            )
        })
        .unzip::<_, _, Vec<_>, Vec<_>>();

    // 4. Create Packet Batches
    debug!("4. creating packet batches...");
    let packet_batches = to_packet_batches(&txs, config.send_chunk_size);
    let packet_batches = packet_batches
        .into_iter()
        .map(|batch| (batch, vec![1u8]))
        .collect::<Vec<_>>();

    let packet_batches = convert_from_old_verified(packet_batches);

    // 5. Send the Packet Batches
    debug!("5. sending packet batches...");
    let mut execute_batches_and_receive_results_elapsed =
        Measure::start("execute_batches_and_receive_results_elapsed");
    non_vote_sender
        .send(BankingPacketBatch::new((packet_batches, None)))
        .unwrap();

    // 6. Ensure all transaction statuses were received
    let mut previous_num_received = 0;
    loop {
        let num_received = tx_received_counter.load(Ordering::Relaxed);
        if num_received == config.num_transactions {
            // eprintln!("\n");
            break;
        }
        if num_received != previous_num_received {
            previous_num_received = num_received;
            // eprint!("{} ", num_received);
        }
    }
    drop(non_vote_sender);
    banking_stage.join().unwrap();
    tx_status_thread.join().unwrap();

    execute_batches_and_receive_results_elapsed.stop();

    assert_eq!(
        tx_received_counter.load(Ordering::Relaxed),
        config.num_transactions
    );

    //      1 transactions, CHUNK_SIZE=100 1 payers.   (execute_and_receive_tx_results:   3ms)
    //      2 transactions, CHUNK_SIZE=100 2 payers.   (execute_and_receive_tx_results:   5ms)
    //      5 transactions, CHUNK_SIZE=100 5 payers.   (execute_and_receive_tx_results: 5ms)
    //     10 transactions, CHUNK_SIZE=100 6 payers.   (execute_and_receive_tx_results: 6ms)

    BenchmarkTransactionsResult {
        num_transactions: config.num_transactions,
        num_payers: config.num_payers,
        send_chunk_size: config.send_chunk_size,
        batch_chunk_size: config.batch_chunk_size,
        execute_batches_and_receive_results_elapsed_ms: execute_batches_and_receive_results_elapsed
            .as_ms(),
    }
}
