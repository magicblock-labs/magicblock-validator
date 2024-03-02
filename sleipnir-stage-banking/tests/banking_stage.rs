use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::Instant,
};

use crossbeam_channel::unbounded;
use log::{debug, info};
use serde::Serialize;
use sleipnir_bank::{
    bank::Bank,
    bank_dev_utils::{init_logger, transactions::create_funded_accounts},
    genesis_utils::{create_genesis_config, GenesisConfigInfo},
};
use sleipnir_stage_banking::{
    banking_stage::BankingStage, packet::BankingPacketBatch,
    transport::banking_tracer::BankingTracer,
};
use sleipnir_transaction_status::{TransactionStatusMessage, TransactionStatusSender};
use solana_measure::measure::Measure;
use solana_perf::packet::{to_packet_batches, PacketBatch};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signature},
    system_transaction,
};

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
    tx_funded: Arc<AtomicU64>,
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
            // Each status has exactly one transaction
            for balance in &batch.balances.post_balances {
                let funded = balance[1];
                tx_funded.store(funded, Ordering::Relaxed);
            }
        }
    });
    (transaction_status_sender, tx_status_thread)
}

fn track_transaction_sigs(
    tx_received_counter: Arc<AtomicU64>,
    sigs: Arc<RwLock<Vec<Signature>>>,
) -> (Option<TransactionStatusSender>, std::thread::JoinHandle<()>) {
    let (transaction_status_sender, transaction_status_receiver) = unbounded();
    let transaction_status_sender = Some(TransactionStatusSender {
        sender: transaction_status_sender,
    });
    let tx_status_thread = std::thread::spawn(move || {
        let transaction_status_receiver = transaction_status_receiver;
        while let Ok(TransactionStatusMessage::Batch(batch)) = transaction_status_receiver.recv() {
            tx_received_counter.fetch_add(batch.transactions.len() as u64, Ordering::Relaxed);
            for tx in batch.transactions {
                let mut sigs = sigs.write().unwrap();
                sigs.push(*tx.signature());
            }
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
fn test_banking_stage_with_transaction_status_sender_tracking_signatures() {
    init_logger();
    solana_logger::setup();

    const SEND_CHUNK_SIZE: usize = 100;

    let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);
    let start_hash = bank.last_blockhash();
    let bank = Arc::new(bank);

    let banking_tracer = BankingTracer::new_disabled();
    let (non_vote_sender, non_vote_receiver) = banking_tracer.create_channel_non_vote();

    // Create the banking stage
    debug!("Creating banking stage...");

    let receive_results_counter = Arc::<AtomicU64>::default();
    let signatures = Arc::<RwLock<Vec<Signature>>>::default();
    let (transaction_status_sender, tx_status_thread) =
        track_transaction_sigs(receive_results_counter.clone(), signatures.clone());
    let banking_stage = BankingStage::new(
        non_vote_receiver,
        transaction_status_sender,
        LOG_MSGS_BYTE_LIMT,
        bank.clone(),
        None,
    );

    // Create Transactions
    debug!("Creating transactions...");
    let fully_funded_tx = {
        let payer = create_funded_accounts(&bank, 1 as usize, Some(LAMPORTS_PER_SOL)).remove(0);
        let to = solana_sdk::pubkey::Pubkey::new_unique();
        system_transaction::transfer(&payer, &to, 890_880_000, start_hash)
    };
    let not_fully_funded_tx = {
        let payer = create_funded_accounts(&bank, 1 as usize, Some(5000)).remove(0);
        let to = solana_sdk::pubkey::Pubkey::new_unique();
        system_transaction::transfer(&payer, &to, 890_880_000, start_hash)
    };

    // Create Packet Batches
    debug!("Creating packet batches...");
    let txs = &[fully_funded_tx, not_fully_funded_tx];
    let packet_batches = to_packet_batches(txs, SEND_CHUNK_SIZE);
    let packet_batches = packet_batches
        .into_iter()
        .map(|batch| (batch, vec![1u8]))
        .collect::<Vec<_>>();

    let packet_batches = convert_from_old_verified(packet_batches);

    // Send the Packet Batches
    debug!("Sending packet batches...");
    non_vote_sender
        .send(BankingPacketBatch::new((packet_batches, None)))
        .unwrap();

    // Wait for all txs to be received
    while receive_results_counter.load(Ordering::Relaxed) < txs.len() as u64 {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Shut things down
    drop(non_vote_sender);
    banking_stage.join().unwrap();
    tx_status_thread.join().unwrap();

    // Check the tx signatures
    let signatures = signatures.read().unwrap();
    for sig in signatures.iter() {
        let status = bank.get_signature_status(sig);
        debug!("sig: {:?} - {:?} ", sig, status);
    }

    assert_eq!(
        receive_results_counter.load(Ordering::Relaxed),
        txs.len() as u64
    );
    let successes = signatures
        .iter()
        .filter(|sig| bank.get_signature_status(sig).unwrap().is_ok())
        .count();
    let failures = signatures
        .iter()
        .filter(|sig| bank.get_signature_status(sig).unwrap().is_err())
        .count();

    assert_eq!(successes, 1);
    assert_eq!(failures, 1);
}

#[test]
fn test_banking_stage_transfer_from_non_existing_account() {
    init_logger();
    solana_logger::setup();

    const SEND_CHUNK_SIZE: usize = 100;

    let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);
    let start_hash = bank.last_blockhash();
    let bank = Arc::new(bank);

    let banking_tracer = BankingTracer::new_disabled();
    let (non_vote_sender, non_vote_receiver) = banking_tracer.create_channel_non_vote();

    // Create the banking stage
    debug!("Creating banking stage...");

    let receive_results_counter = Arc::<AtomicU64>::default();
    let signatures = Arc::<RwLock<Vec<Signature>>>::default();
    let (transaction_status_sender, tx_status_thread) =
        track_transaction_sigs(receive_results_counter.clone(), signatures.clone());
    let banking_stage = BankingStage::new(
        non_vote_receiver,
        transaction_status_sender,
        LOG_MSGS_BYTE_LIMT,
        bank.clone(),
        None,
    );

    // Create Transactions
    debug!("Creating transactions...");
    let not_existing = {
        let payer = Keypair::new();
        let to = solana_sdk::pubkey::Pubkey::new_unique();
        system_transaction::transfer(&payer, &to, 890_880_000, start_hash)
    };

    // Create Packet Batches
    debug!("Creating packet batches...");
    let txs = &[not_existing];
    let packet_batches = to_packet_batches(txs, SEND_CHUNK_SIZE);
    let packet_batches = packet_batches
        .into_iter()
        .map(|batch| (batch, vec![1u8]))
        .collect::<Vec<_>>();

    let packet_batches = convert_from_old_verified(packet_batches);

    // Send the Packet Batches
    debug!("Sending packet batches...");
    non_vote_sender
        .send(BankingPacketBatch::new((packet_batches, None)))
        .unwrap();

    // Wait for all txs to be received
    let mut counter = 0;
    while receive_results_counter.load(Ordering::Relaxed) < txs.len() as u64 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        counter += 1;
        // I verified that a transaction with a payer that doesn't exist at all just gets
        // dropped (instead of failing) and we have no way of verifying this at this level
        // So we just wait here long enough to ensure we didn't hear back
        if counter > 10 {
            break;
        }
    }

    // Shut things down
    drop(non_vote_sender);
    banking_stage.join().unwrap();
    tx_status_thread.join().unwrap();

    // Check the tx signatures
    assert_eq!(receive_results_counter.load(Ordering::Relaxed), 0);
}

// -----------------
// Benchmarking
// -----------------
#[test]
fn test_banking_stage_with_transaction_status_sender_perf() {
    init_logger();
    solana_logger::setup();

    const SEND_CHUNK_SIZE: usize = 100;
    // We saw clearly that max thread count is ideal (tried 1..6)
    let thread_count = std::env::var("THREAD_COUNT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(6);

    let num_transactions: Vec<u64> = vec![1, 2, 5, 10, 100, 1000, 10_000];
    let batch_sizes: Vec<usize> = vec![1, 2, 4, 8, 16, 32, 64, 128];

    let permutations = num_transactions
        .iter()
        .flat_map(|num_transactions| {
            batch_sizes
                .iter()
                .map(|batch_size| (*num_transactions, *batch_size))
        })
        .collect::<Vec<_>>();

    let mut results: Vec<BenchmarkTransactionsResult> = Vec::with_capacity(permutations.len());
    for (num_transactions, batch_size) in permutations {
        let num_payers: u64 = (num_transactions / thread_count).min(thread_count).max(1);
        let config = BenchmarkTransactionsConfig {
            num_transactions,
            num_payers,
            send_chunk_size: SEND_CHUNK_SIZE,
            batch_chunk_size: batch_size,
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
    let tx_funded = Arc::<AtomicU64>::default();
    let (transaction_status_sender, tx_status_thread) =
        watch_transaction_status(tx_received_counter.clone(), tx_funded.clone());
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
                // We're abusing the post balance as tx id
                system_transaction::transfer(payer, &to, 890_880_000 + idx, start_hash),
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
    let instant_clock = Instant::now();
    non_vote_sender
        .send(BankingPacketBatch::new((packet_batches, None)))
        .unwrap();

    // 6. Ensure all transaction statuses were received
    let mut previous_num_received = 0;
    let mut previous_funded = 0;
    let mut times = Vec::with_capacity(config.num_transactions as usize);
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
        let funded = tx_funded.load(Ordering::Relaxed);
        let elapsed = instant_clock.elapsed();
        if funded != previous_funded {
            previous_funded = funded;
            times.push((elapsed.as_millis(), elapsed.as_micros()));
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

    if !times.is_empty() {
        let min_ms = times.iter().map(|(ms, _)| ms).min().unwrap();
        let min_us = times.iter().map(|(_, ns)| ns).min().unwrap();
        let max_ms = times.iter().map(|(ms, _)| ms).max().unwrap();
        let max_us = times.iter().map(|(_, ns)| ns).max().unwrap();
        let average_ms = times.iter().map(|(ms, _)| ms).sum::<u128>() / times.len() as u128;
        let average_us = times.iter().map(|(_, us)| us).sum::<u128>() / times.len() as u128;
        debug!(
            "txs {}, batch_size: {} -> min: {}ms {}us, max: {}ms {}us, average: {}ms {}us",
            config.num_transactions,
            config.batch_chunk_size,
            min_ms,
            min_us,
            max_ms,
            max_us,
            average_ms,
            average_us
        );
    }

    BenchmarkTransactionsResult {
        num_transactions: config.num_transactions,
        num_payers: config.num_payers,
        send_chunk_size: config.send_chunk_size,
        batch_chunk_size: config.batch_chunk_size,
        execute_batches_and_receive_results_elapsed_ms: execute_batches_and_receive_results_elapsed
            .as_ms(),
    }
}
