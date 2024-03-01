use std::sync::Arc;

use crossbeam_channel::unbounded;
use log::{debug, error, info};
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
use sleipnir_transaction_status::TransactionStatusSender;
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

fn watch_transaction_status() -> (Option<TransactionStatusSender>, std::thread::JoinHandle<()>) {
    let (transaction_status_sender, transaction_status_receiver) = unbounded();
    let transaction_status_sender = Some(TransactionStatusSender {
        sender: transaction_status_sender,
    });
    let tx_status_thread = std::thread::spawn(move || {
        let transaction_status_receiver = transaction_status_receiver;
        while let Ok(msg) = transaction_status_receiver.recv() {
            debug!("received msg: {:#?}", msg);
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

    let banking_stage = BankingStage::new(non_vote_receiver, None, LOG_MSGS_BYTE_LIMT, bank);
    drop(non_vote_sender);
    banking_stage.join().unwrap();
}

#[test]
fn test_banking_stage_with_transaction_status_sender() {
    init_logger();
    solana_logger::setup();
    let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(u64::MAX);

    let bank = Bank::new_for_tests(&genesis_config);
    let start_hash = bank.last_blockhash();
    let bank = Arc::new(bank);

    let banking_tracer = BankingTracer::new_disabled();
    let (non_vote_sender, non_vote_receiver) = banking_tracer.create_channel_non_vote();

    // const NUM_TRANSACTIONS: u64 = 10_000;
    const NUM_TRANSACTIONS: u64 = 2;
    let num_payers: u64 = if NUM_TRANSACTIONS > 128 {
        256
    } else {
        NUM_TRANSACTIONS.next_power_of_two()
    };
    const CHUNK_SIZE: usize = 100;

    // 1. Fund an account so we can send 2 good transactions in a single batch.
    debug!("1. funding payers...");
    let payers = create_funded_accounts(&bank, num_payers as usize, Some(LAMPORTS_PER_SOL));

    // 2. Create the banking stage
    debug!("2. creating banking stage...");

    let (transaction_status_sender, tx_status_thread) = watch_transaction_status();
    let banking_stage = BankingStage::new(
        non_vote_receiver,
        transaction_status_sender,
        LOG_MSGS_BYTE_LIMT,
        bank,
    );

    // 3. Create Transactions
    debug!("3. creating transactions...");
    let (accs, txs) = (0..NUM_TRANSACTIONS)
        .map(|idx| {
            let payer = &payers[(idx % num_payers) as usize];
            let to = solana_sdk::pubkey::Pubkey::new_unique();
            (
                to,
                system_transaction::transfer(payer, &to, 890_880_000, start_hash),
            )
        })
        .unzip::<_, _, Vec<_>, Vec<_>>();

    // 4. Create Packet Batches
    debug!("4. creating packet batches...");
    let packet_batches = to_packet_batches(&txs, CHUNK_SIZE);
    let packet_batches = packet_batches
        .into_iter()
        .map(|batch| (batch, vec![1u8]))
        .collect::<Vec<_>>();

    let packet_batches = convert_from_old_verified(packet_batches);

    // 5. Send the Packet Batches
    debug!("5. sending packet batches...");
    let mut execute_batches_elapsed = Measure::start("execute_batches_elapsed");
    non_vote_sender
        .send(BankingPacketBatch::new((packet_batches, None)))
        .unwrap();

    drop(non_vote_sender);
    banking_stage.join().unwrap();

    // 100K transactions, CHUNK_SIZE=100
    // 2 payers:  7,466ms
    // 64 payers: 5,308ms
    execute_batches_elapsed.stop();
    // 6. Ensure all transaction statuses were received
    tx_status_thread.join().unwrap();

    info!(
        "{}K transactions, CHUNK_SIZE={} {} payers:  {}ms",
        NUM_TRANSACTIONS / 1_000,
        CHUNK_SIZE,
        num_payers,
        execute_batches_elapsed.as_ms()
    );
}
